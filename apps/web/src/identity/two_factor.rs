//! Member two-factor authentication: authenticator (TOTP, RFC 6238) and recovery codes.
//!
//! - Once enabled, password sign-in no longer issues a session directly; it issues a [`PENDING_TTL`]-second encrypted
//!   "awaiting second step" Cookie instead, and the session is issued only after a correct code or recovery code.
//!   Recovering and resetting the password does not turn off two-factor authentication, it only resets the failure
//!   count — except for an account whose email is unverified, where recovery claims the account and turns it off
//!   ([`Identity::remove_two_factor_on_claim`]); such an account cannot turn it on in the first place.
//! - The authenticator secret is stored encrypted with the key in the private configuration `[identity.two_factor]`,
//!   with the AAD bound to the uid; recovery codes are stored only as digests.
//! - A code is accepted only for a time step larger than the last one used (the same code cannot be used twice); the
//!   failure count is stored in the database, so it survives restarts and is consistent across processes; during
//!   a lockout neither codes nor recovery codes are accepted.
//! - Enabling bumps the credentials version by 1, signing out other devices, and the current device is re-signed in the
//!   same response; API tokens and feed URLs are left alone.
//! - Statements that write the secret ciphertext and recovery code digests are handed directly to SQLx, bypassing the
//!   debug panel's parameter recording.
use crate::db::Tx;
use std::fmt::Write as _;

use hmac::{Hmac, Mac};
use sha1::Sha1;

use super::*;

/// TOTP time step (seconds) and digit count: the RFC 6238 defaults, supported by all mainstream authenticator apps.
pub const STEP_SECONDS: i64 = 30;
const DIGITS: u32 = 6;
/// Authenticator secret of 20 bytes (160 bits), the length RFC 4226 recommends.
const SECRET_BYTES: usize = 20;
/// Number of recovery codes in one batch.
pub const RECOVERY_CODE_COUNT: usize = 10;
/// Each recovery code has 16 characters from an alphabet without the easily confused 0 1 i l o, 31 characters in all,
/// about 79 random bits: even a database leak cannot reverse them.
const RECOVERY_CODE_LENGTH: usize = 16;
const RECOVERY_ALPHABET: &[u8] = b"23456789abcdefghjkmnpqrstuvwxyz";
/// At most 10 minutes between the two steps of enabling the authenticator (getting the secret to entering the first
/// code).
const SETUP_TTL: i64 = 600;
/// Lifetime of the "password correct, awaiting second step" state.
pub const PENDING_TTL: i64 = 300;
/// New recovery codes are shown only once: kept in an encrypted Cookie for this long, cleared as soon as the display
/// page reads it.
pub const REVEAL_TTL: i64 = 600;
const PENDING_AAD: &[u8] = b"gonof.second-factor-pending.v1";
const SETUP_AAD: &[u8] = b"gonof.totp-setup.v1";
const REVEAL_AAD: &[u8] = b"gonof.recovery-reveal.v1";
/// The input box accepts at most this many characters; anything longer is treated as incorrect outright.
const ANSWER_MAX_CHARS: usize = 64;

pub(crate) const ENABLE_LOG: &str = "identity.two_factor.enable";
pub(crate) const DISABLE_LOG: &str = "identity.two_factor.disable";
pub(crate) const REGENERATE_LOG: &str = "identity.two_factor.recovery.regenerate";
pub(crate) const LOCKOUT_LOG: &str = "identity.two_factor.lockout";
pub(crate) const RESET_LOG: &str = "identity.two_factor.reset";

/// Security alert emails: cannot be turned off, sent only to verified email addresses, at most
/// [`NOTICE_HOURLY_LIMIT`] per account per hour.
pub(crate) const NOTICE_EVENT: &str = "security_notice";
pub(crate) const NOTICE_HOURLY_LIMIT: i64 = 10;

fn setup_expired() -> IdentityError {
    IdentityError::Validation(crate::i18n::current().settings_error_setup_expired())
}

fn wrong_setup_code() -> IdentityError {
    IdentityError::Validation(crate::i18n::current().settings_error_setup_code())
}

#[derive(Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TwoFactorConfig {
    pub active_key: String,
    pub previous_keys: Vec<String>,
}

impl fmt::Debug for TwoFactorConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TwoFactorConfig")
            .field("active_key", &"[redacted]")
            .field("previous_keys", &"[redacted]")
            .finish()
    }
}

impl TwoFactorConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            cookie::valid_key(&self.active_key),
            "identity.two_factor.active_key is missing or invalid: set a random 64-digit hex key (32 bytes, for example from openssl rand -hex 32) in the [identity.two_factor] section of the private configuration (or the GONOF_TWO_FACTOR_KEY environment variable). It encrypts the members' authenticator secrets, must be kept for the long term and must differ from the cookie keys"
        );
        anyhow::ensure!(
            self.previous_keys.len() <= 3,
            "at most 3 identity.two_factor.previous_keys are kept"
        );
        let mut seen = std::collections::HashSet::from([self.active_key.to_ascii_lowercase()]);
        for key in &self.previous_keys {
            anyhow::ensure!(
                cookie::valid_key(key),
                "the keys in identity.two_factor.previous_keys must be 64 hex digits"
            );
            anyhow::ensure!(
                seen.insert(key.to_ascii_lowercase()),
                "two-step verification keys must not repeat"
            );
        }
        Ok(())
    }

    /// All keys (current first), for comparison with the Cookie keys.
    pub(super) fn keys(&self) -> impl Iterator<Item = &String> {
        std::iter::once(&self.active_key).chain(&self.previous_keys)
    }

    /// Encryption and decryption reuse the Cookie AES-256-GCM implementation (same key IDs, rotation and checks), just
    /// with a different set of keys.
    fn ring(&self) -> cookie::CookieConfig {
        cookie::CookieConfig {
            active_key: self.active_key.clone(),
            previous_keys: self.previous_keys.clone(),
        }
    }
}

/// Lottery redemption codes share this key set with authenticator secrets: the AAD differs, so neither can decrypt the
/// other; it distinguishes by tid, prize number and slot number, so a whole row moved to another slot cannot be
/// decrypted either. Lose this key and the redemption codes cannot be decrypted either.
impl TwoFactorConfig {
    pub fn seal_lottery_code(
        &self,
        tid: i64,
        prize: i64,
        slot: i64,
        code: &str,
    ) -> anyhow::Result<String> {
        self.ring()
            .seal_value(&lottery_code_aad(tid, prize, slot), &code)
    }

    /// Returns `None` when it cannot be decrypted (key deleted, moved to another slot, tampered with).
    pub fn open_lottery_code(&self, tid: i64, prize: i64, slot: i64, raw: &str) -> Option<String> {
        self.ring()
            .open_value(&lottery_code_aad(tid, prize, slot), raw)
    }

    /// Whether the ciphertext is already encrypted with the current key (`rekey-two-factor` skips it on that basis).
    pub fn lottery_code_is_current(&self, raw: &str) -> bool {
        self.ring().uses_active_key(raw)
    }
}

fn lottery_code_aad(tid: i64, prize: i64, slot: i64) -> Vec<u8> {
    format!("gonof.lottery-code.v1:{tid}:{prize}:{slot}").into_bytes()
}

fn secret_aad(uid: i64) -> Vec<u8> {
    format!("gonof.totp-secret.v1:{uid}").into_bytes()
}

fn seal_secret(config: &TwoFactorConfig, uid: i64, secret: &[u8]) -> Result<String> {
    Ok(config.ring().seal_value(&secret_aad(uid), &hex(secret))?)
}

/// Returns `None` when it cannot be decrypted (key deleted, whole row moved to another account, tampered with).
fn open_secret(config: &TwoFactorConfig, uid: i64, raw: &str) -> Option<Vec<u8>> {
    let text: String = config.ring().open_value(&secret_aad(uid), raw)?;
    decode_hex(&text).filter(|bytes| bytes.len() == SECRET_BYTES)
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect()
}

/// RFC 4226 HMAC-SHA1 dynamic truncation, returning a 31-bit integer.
fn truncated(secret: &[u8], counter: u64) -> u32 {
    let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(secret).expect("HMAC takes any key length");
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = usize::from(digest[digest.len() - 1] & 0x0f);
    u32::from_be_bytes([
        digest[offset] & 0x7f,
        digest[offset + 1],
        digest[offset + 2],
        digest[offset + 3],
    ])
}

fn code_at(secret: &[u8], step: i64) -> String {
    let counter = u64::try_from(step).unwrap_or(0);
    format!(
        "{:0width$}",
        truncated(secret, counter) % 10_u32.pow(DIGITS),
        width = DIGITS as usize
    )
}

/// When the entered 6-digit code falls within 1 step either side and its time step is larger than the last one used,
/// returns the corresponding time step.
fn accepted_step(secret: &[u8], code: &str, now: i64, last_step: i64) -> Option<i64> {
    let current = now.div_euclid(STEP_SECONDS);
    let mut accepted = None;
    // All three time steps are computed before judging, and comparison is constant-time, so the duration does not vary
    // with the match position.
    for step in current - 1..=current + 1 {
        let matches = bool::from(code_at(secret, step).as_bytes().ct_eq(code.as_bytes()));
        if matches && step > last_step {
            accepted = Some(step);
        }
    }
    accepted
}

/// RFC 4648 Base32; neither the authenticator app's secret nor the `otpauth://` URL carries padding.
fn base32(bytes: &[u8], padded: bool) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let (mut buffer, mut bits) = (0_u32, 0_u32);
    for &byte in bytes {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(char::from(ALPHABET[((buffer >> bits) & 31) as usize]));
        }
        buffer &= (1 << bits) - 1;
    }
    if bits > 0 {
        out.push(char::from(ALPHABET[((buffer << (5 - bits)) & 31) as usize]));
    }
    if padded {
        while !out.len().is_multiple_of(8) {
            out.push('=');
        }
    }
    out
}

/// The issuer written into the authenticator app is the site name (`crate::site::site_name`), so each site's entry
/// carries its own name; the account label is `issuer:username`.
fn otpauth_uri(issuer: &str, username: &str, secret: &str) -> String {
    use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
    let label = format!("{issuer}:{username}");
    format!(
        "otpauth://totp/{}?secret={secret}&issuer={}&algorithm=SHA1&digits={DIGITS}&period={STEP_SECONDS}",
        utf8_percent_encode(&label, NON_ALPHANUMERIC),
        utf8_percent_encode(issuer, NON_ALPHANUMERIC)
    )
}

/// The QR code is drawn as inline SVG, usable without JS. Black modules on white are hard-coded into the image:
/// scanning needs a fixed high contrast that does not change with the dark theme.
fn qr_svg(text: &str) -> Result<String> {
    const BORDER: i32 = 4;
    let qr = qrcodegen::QrCode::encode_text(text, qrcodegen::QrCodeEcc::Medium).map_err(|_| {
        IdentityError::Internal(anyhow::anyhow!(
            "the authenticator address is too long for a QR code"
        ))
    })?;
    let size = qr.size();
    let dimension = size + BORDER * 2;
    let mut path = String::new();
    for y in 0..size {
        for x in 0..size {
            if qr.get_module(x, y) {
                let _ = write!(path, "M{},{}h1v1h-1z", x + BORDER, y + BORDER);
            }
        }
    }
    let label = crate::i18n::Markup::text(crate::i18n::current().settings_two_factor_qr_label());
    Ok(format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {dimension} {dimension}" role="img" aria-label="{label}" shape-rendering="crispEdges"><rect width="{dimension}" height="{dimension}" fill="#fff"/><path d="{path}" fill="#000"/></svg>"##
    ))
}

fn group(value: &str, size: usize, separator: &str) -> String {
    value
        .as_bytes()
        .chunks(size)
        .map(|chunk| std::str::from_utf8(chunk).unwrap_or_default())
        .collect::<Vec<_>>()
        .join(separator)
}

fn new_recovery_codes() -> Vec<String> {
    (0..RECOVERY_CODE_COUNT)
        .map(|_| {
            (0..RECOVERY_CODE_LENGTH)
                .map(|_| char::from(RECOVERY_ALPHABET[OsRng.gen_range(0..RECOVERY_ALPHABET.len())]))
                .collect()
        })
        .collect()
}

/// For display: `xxxx-xxxx-xxxx-xxxx`.
pub fn display_recovery_code(code: &str) -> String {
    group(code, 4, "-")
}

/// Input is case-insensitive; whitespace and hyphens are ignored.
fn normalized_recovery_code(input: &str) -> Option<String> {
    let code: String = input
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_lowercase())
        .collect();
    (code.len() == RECOVERY_CODE_LENGTH && code.bytes().all(|b| RECOVERY_ALPHABET.contains(&b)))
        .then_some(code)
}

fn recovery_hash(uid: i64, code: &str) -> String {
    token_hash(&format!("gonof.recovery-code.v1:{uid}:{code}"))
}

/// What the second-step input box received: 6 digits are a code; if it normalizes to 16 characters it is a recovery
/// code.
enum Answer {
    Totp(String),
    Recovery(String),
    Invalid,
}

fn answer(input: &str) -> Answer {
    if input.chars().count() > ANSWER_MAX_CHARS {
        return Answer::Invalid;
    }
    let compact: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.len() == DIGITS as usize && compact.bytes().all(|b| b.is_ascii_digit()) {
        return Answer::Totp(compact);
    }
    normalized_recovery_code(input).map_or(Answer::Invalid, Answer::Recovery)
}

/// Lockout duration in seconds at the 5th, 10th, 15th… consecutive failure: 15 minutes, 1 hour, then 24 hours each
/// time.
fn lock_seconds(failed_attempts: i64) -> i64 {
    if failed_attempts <= 0 || failed_attempts % 5 != 0 {
        return 0;
    }
    match failed_attempts / 5 {
        1 => 900,
        2 => 3600,
        _ => 86_400,
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingClaims {
    uid: i64,
    credentials_version: i64,
    expires_at: i64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetupClaims {
    uid: i64,
    credentials_version: i64,
    /// Hex of the new authenticator secret; only in the hidden field of the confirmation form, encrypted with the Cookie
    /// key.
    secret: String,
    expires_at: i64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RevealClaims {
    uid: i64,
    codes: Vec<String>,
    expires_at: i64,
}

/// Password correct, awaiting second step: `token` is the ciphertext written into the Cookie.
pub struct PendingLogin {
    pub token: String,
}

pub enum LoginOutcome {
    Session(Box<SessionGrant>),
    SecondFactor(PendingLogin),
}

#[derive(Clone, Debug)]
pub struct TwoFactorStatus {
    pub enabled_at: i64,
    pub totp_enabled_at: i64,
    pub recovery_remaining: i64,
    pub locked_until: i64,
}

/// Result of the first step of enabling the authenticator: the QR code, the secret for manual entry and the hidden
/// token of the confirmation form.
pub struct TotpSetup {
    pub token: String,
    /// Base32 secret in groups of 4 separated by spaces, for manual entry.
    pub secret: String,
    pub uri: String,
    pub qr_svg: String,
}

pub enum SecondFactorOutcome {
    Granted {
        grant: Box<SessionGrant>,
        /// Number left when signing in with a recovery code; `None` when a code was used.
        recovery_remaining: Option<i64>,
    },
    Rejected,
    Locked {
        until: i64,
    },
    /// The authenticator secret cannot be decrypted (an old key was removed from the private configuration too early):
    /// not counted as a failure, only recovery codes work.
    Unavailable,
}

pub struct TwoFactorEnabled {
    pub grant: SessionGrant,
    /// Ciphertext of the display Cookie for the new recovery codes.
    pub reveal: String,
}

pub enum TwoFactorDisable {
    Disabled,
    Rejected,
    Locked { until: i64 },
    Unavailable,
}

#[derive(Debug, Default)]
pub struct RekeyReport {
    pub rekeyed: usize,
    pub current: usize,
    pub unreadable: Vec<i64>,
}

struct FactorRow {
    totp_secret: Option<String>,
    totp_step: i64,
    failed_attempts: i64,
    locked_until: i64,
}

enum FactorCheck {
    Totp,
    Recovery { remaining: i64 },
    Rejected,
    Locked(i64),
    Unavailable,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SecurityNoticeMail {
    pub uid: i64,
    /// Row ID of the log this action wrote; what the email says is decided by that log's type.
    pub log_id: i64,
    /// Digest of the recipient email address at enqueue time; delivery is abandoned if the email has changed.
    pub email_hash: String,
}

pub(crate) fn notice_key_range(uid: i64) -> (String, String) {
    (
        format!("{NOTICE_EVENT}:{uid}:"),
        format!("{NOTICE_EVENT}:{uid};"),
    )
}

/// The caller holds the member row lock and has already written the `log_id` log. Not sent when the email is
/// unverified; not enqueued beyond the hourly limit.
pub(super) async fn enqueue_notice(
    db: &Database,
    tx: &mut Tx<'_>,
    member: &Member,
    log_id: i64,
    now: i64,
) -> Result<()> {
    let Some(email) = member.email.as_deref().filter(|email| !email.is_empty()) else {
        return Ok(());
    };
    if !member.email_verified {
        return Ok(());
    }
    let (low, high) = notice_key_range(member.uid);
    let recent: i64 = crate::db::query_scalar(
        "SELECT COUNT(*) FROM gonof_common_outbox WHERE event_key>=? AND event_key<? AND dateline>=?",
    )
    .bind(&low)
    .bind(&high)
    .bind(now.saturating_sub(3600))
    .fetch_one(db.observe(&mut **tx))
    .await?;
    if recent >= NOTICE_HOURLY_LIMIT {
        return Ok(());
    }
    let payload = serde_json::to_string(&SecurityNoticeMail {
        uid: member.uid,
        log_id,
        email_hash: token_hash(email),
    })
    .context("failed to build the security notice event")?;
    crate::db::query("INSERT INTO gonof_common_outbox(event_key,event_type,aggregate_type,aggregate_id,payload,dateline,available_at) VALUES(?,?,'member',?,?,?,?)")
        .bind(format!("{low}{log_id}"))
        .bind(NOTICE_EVENT)
        .bind(member.uid)
        .bind(payload)
        .bind(now)
        .bind(now)
        .execute(db.observe(&mut **tx))
        .await?;
    Ok(())
}

fn client_data(metadata: &SessionMetadata) -> serde_json::Value {
    serde_json::json!({"user_agent": metadata.user_agent, "client_ip": metadata.client_ip})
}

impl Identity {
    /// Password correct but the account has two-factor authentication enabled: issues the awaiting-second-step
    /// credential, not a session.
    pub(super) fn pending_login(&self, member: &Member, now: i64) -> Result<PendingLogin> {
        let token = self.config.cookie.seal_value(
            PENDING_AAD,
            &PendingClaims {
                uid: member.uid,
                credentials_version: member.credentials_version,
                expires_at: now + PENDING_TTL,
            },
        )?;
        Ok(PendingLogin { token })
    }

    /// Whether the awaiting-second-step credential is still valid (looks only at the ciphertext and expiry; the account
    /// status is checked again under the lock in [`Self::complete_login`]).
    pub fn pending_login_valid(&self, raw: &str) -> bool {
        self.pending_claims(raw, now()).is_some()
    }

    fn pending_claims(&self, raw: &str, now: i64) -> Option<PendingClaims> {
        self.config
            .cookie
            .open_value::<PendingClaims>(PENDING_AAD, raw)
            .filter(|claims| {
                claims.uid > 0
                    && claims.credentials_version > 0
                    && claims.expires_at > now
                    && claims.expires_at <= now + PENDING_TTL
            })
    }

    /// The caller holds the member row lock.
    pub(super) async fn has_two_factor_locked(&self, tx: &mut Tx<'_>, uid: i64) -> Result<bool> {
        let found: Option<i64> = crate::db::query_scalar(
            "SELECT uid FROM gonof_common_member_two_factor WHERE uid = ? FOR UPDATE",
        )
        .bind(uid)
        .fetch_optional(self.db.observe(&mut **tx))
        .await?;
        Ok(found.is_some())
    }

    async fn factor_locked(&self, tx: &mut Tx<'_>, uid: i64) -> Result<Option<FactorRow>> {
        let row = crate::db::query(
            "SELECT totp_secret, totp_step, failed_attempts, locked_until
             FROM gonof_common_member_two_factor WHERE uid = ? FOR UPDATE",
        )
        .bind(uid)
        .fetch_optional(self.db.observe(&mut **tx))
        .await?;
        row.map(|row| {
            Ok(FactorRow {
                totp_secret: row.try_get("totp_secret")?,
                totp_step: row.try_get("totp_step")?,
                failed_attempts: row.try_get("failed_attempts")?,
                locked_until: row.try_get("locked_until")?,
            })
        })
        .transpose()
    }

    /// Checks the second-step input. The caller holds the member row lock and the two-factor row lock; the failure count
    /// and lockout are written back here, and the caller must commit the transaction whatever the result.
    async fn check_factor(
        &self,
        tx: &mut Tx<'_>,
        member: &Member,
        row: &FactorRow,
        input: &str,
        metadata: &SessionMetadata,
        now: i64,
    ) -> Result<FactorCheck> {
        let uid = member.uid;
        if row.locked_until > now {
            return Ok(FactorCheck::Locked(row.locked_until));
        }
        match answer(input) {
            Answer::Totp(code) => {
                let Some(raw) = row.totp_secret.as_deref() else {
                    return Ok(FactorCheck::Unavailable);
                };
                let Some(secret) = open_secret(&self.config.two_factor, uid, raw) else {
                    tracing::error!(
                        uid,
                        "authenticator secret cannot be decrypted: check whether [identity.two_factor] dropped an old key still in use"
                    );
                    return Ok(FactorCheck::Unavailable);
                };
                if let Some(step) = accepted_step(&secret, &code, now, row.totp_step) {
                    // Still on an old key, so re-encrypt with the current key while at it (rotation, see
                    // docs/install.md).
                    let resealed = if self.config.two_factor.ring().uses_active_key(raw) {
                        raw.to_owned()
                    } else {
                        seal_secret(&self.config.two_factor, uid, &secret)?
                    };
                    let changed = crate::db::query(
                        "UPDATE gonof_common_member_two_factor
                         SET totp_step = ?, totp_secret = ?, failed_attempts = 0, locked_until = 0
                         WHERE uid = ? AND totp_step < ?",
                    )
                    .bind(step)
                    .bind(&resealed)
                    .bind(uid)
                    .bind(step)
                    .execute(&mut **tx)
                    .await?
                    .rows_affected();
                    if changed == 1 {
                        return Ok(FactorCheck::Totp);
                    }
                }
            }
            Answer::Recovery(code) => {
                let used = crate::db::query(
                    "DELETE FROM gonof_common_member_recovery_code WHERE uid = ? AND code_hash = ?",
                )
                .bind(uid)
                .bind(recovery_hash(uid, &code))
                .execute(&mut **tx)
                .await?
                .rows_affected();
                if used == 1 {
                    crate::db::query("UPDATE gonof_common_member_two_factor SET failed_attempts = 0, locked_until = 0 WHERE uid = ?")
                        .bind(uid)
                        .execute(self.db.observe(&mut **tx))
                        .await?;
                    let remaining: i64 = crate::db::query_scalar(
                        "SELECT COUNT(*) FROM gonof_common_member_recovery_code WHERE uid = ?",
                    )
                    .bind(uid)
                    .fetch_one(self.db.observe(&mut **tx))
                    .await?;
                    return Ok(FactorCheck::Recovery { remaining });
                }
            }
            Answer::Invalid => {}
        }
        let failed = row.failed_attempts.saturating_add(1);
        let lock = lock_seconds(failed);
        let locked_until = if lock > 0 { now + lock } else { 0 };
        crate::db::query("UPDATE gonof_common_member_two_factor SET failed_attempts = ?, locked_until = ? WHERE uid = ?")
            .bind(failed)
            .bind(locked_until)
            .bind(uid)
            .execute(self.db.observe(&mut **tx))
            .await?;
        if lock == 0 {
            return Ok(FactorCheck::Rejected);
        }
        let mut data = client_data(metadata);
        data["failed_attempts"] = failed.into();
        data["locked_until"] = locked_until.into();
        let log_id = self.audit(tx, uid, LOCKOUT_LOG, data, now).await?;
        enqueue_notice(&self.db, tx, member, log_id, now).await?;
        Ok(FactorCheck::Locked(locked_until))
    }

    /// Wrap-up of a successful sign-in: issues the session, writes the sign-in log, new-device alert. The caller holds the
    /// member row lock; account settings such as session lifetime are read by the caller before opening the transaction
    /// and passed in, and the transaction does not read the process cache (a miss would query the database through the
    /// read pool and lengthen the write transaction).
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn finish_login(
        &self,
        tx: &mut Tx<'_>,
        member: Member,
        metadata: &SessionMetadata,
        method: Option<&str>,
        recovery_remaining: Option<i64>,
        timestamp: i64,
        account: &crate::runtime::settings::AccountSettings,
    ) -> Result<SessionGrant> {
        let uid = member.uid;
        let mut grant = self.grant_session(member, timestamp, account)?;
        let mut data = client_data(metadata);
        if let Some(method) = method {
            data["method"] = method.into();
        }
        if let Some(remaining) = recovery_remaining {
            data["recovery_remaining"] = remaining.into();
        }
        let log_id = self
            .audit(tx, uid, "identity.login", data, timestamp)
            .await?;
        // Only web sign-ins carry the device marker; a browser without this account's marker is a new device, and the
        // sign-in alert is queued according to preference.
        if let Some(raw) = &metadata.device {
            if !login_alert::known(&self.config.cookie, raw, uid) {
                login_alert::enqueue(&self.db, tx, &grant.member, log_id, timestamp).await?;
            }
            grant.device_marker = Some(login_alert::remember(&self.config.cookie, raw, uid)?);
        }
        if recovery_remaining.is_some() {
            enqueue_notice(&self.db, tx, &grant.member, log_id, timestamp).await?;
        }
        Ok(grant)
    }

    /// Second step: the session is issued only after a correct code or recovery code. An expired awaiting-second-step
    /// credential, changed account status or credentials version, or two-factor authentication having been turned off
    /// all return `Unauthorized`, and the password must be entered again.
    pub async fn complete_login(
        &self,
        pending: &str,
        input: &str,
        metadata: &SessionMetadata,
    ) -> Result<SecondFactorOutcome> {
        let timestamp = now();
        let claims = self
            .pending_claims(pending, timestamp)
            .ok_or(IdentityError::Unauthorized)?;
        let settings = self.settings().await?;
        let mut tx = self.db.begin_write().await?;
        let member = self.member_locked(&mut tx, claims.uid).await?;
        if !member.active() || member.frozen {
            if let Some(message) =
                crate::moderation::frozen_notice(&self.db, &mut tx, member.uid).await?
            {
                return Err(IdentityError::Suspended(message));
            }
            return Err(IdentityError::Unauthorized);
        }
        if member.credentials_version != claims.credentials_version {
            return Err(IdentityError::Unauthorized);
        }
        let Some(row) = self.factor_locked(&mut tx, member.uid).await? else {
            return Err(IdentityError::Unauthorized);
        };
        let outcome = match self
            .check_factor(&mut tx, &member, &row, input, metadata, timestamp)
            .await?
        {
            FactorCheck::Totp => SecondFactorOutcome::Granted {
                grant: Box::new(
                    self.finish_login(
                        &mut tx,
                        member,
                        metadata,
                        Some("totp"),
                        None,
                        timestamp,
                        &settings.account,
                    )
                    .await?,
                ),
                recovery_remaining: None,
            },
            FactorCheck::Recovery { remaining } => SecondFactorOutcome::Granted {
                grant: Box::new(
                    self.finish_login(
                        &mut tx,
                        member,
                        metadata,
                        Some("recovery"),
                        Some(remaining),
                        timestamp,
                        &settings.account,
                    )
                    .await?,
                ),
                recovery_remaining: Some(remaining),
            },
            FactorCheck::Rejected => SecondFactorOutcome::Rejected,
            FactorCheck::Locked(until) => SecondFactorOutcome::Locked { until },
            FactorCheck::Unavailable => SecondFactorOutcome::Unavailable,
        };
        tx.commit().await?;
        Ok(outcome)
    }

    /// Status shown on the account security page. Read only when the account security page opens, not part of the
    /// per-request authentication query.
    pub async fn two_factor_status(&self, uid: i64) -> Result<Option<TwoFactorStatus>> {
        let row = crate::db::query(
            "SELECT enabled_at, totp_enabled_at, locked_until FROM gonof_common_member_two_factor WHERE uid = ?",
        )
        .bind(uid)
        .fetch_optional(self.db.executor())
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let recovery_remaining: i64 = crate::db::query_scalar(
            "SELECT COUNT(*) FROM gonof_common_member_recovery_code WHERE uid = ?",
        )
        .bind(uid)
        .fetch_one(self.db.executor())
        .await?;
        Ok(Some(TwoFactorStatus {
            enabled_at: row.try_get("enabled_at")?,
            totp_enabled_at: row.try_get("totp_enabled_at")?,
            recovery_remaining,
            locked_until: row.try_get("locked_until")?,
        }))
    }

    /// Check under the lock: the session is still valid and the password has not changed since it was verified outside
    /// the lock.
    async fn password_unchanged_locked(
        &self,
        tx: &mut Tx<'_>,
        raw_sid: &str,
        previous: &Option<String>,
    ) -> Result<Member> {
        let LockedAccount { member, password } = self.security_account_locked(tx, raw_sid).await?;
        if &password != previous {
            return Err(IdentityError::Unauthorized);
        }
        Ok(member)
    }

    /// First step of enabling: verifies the current password and generates a new authenticator secret. Nothing is written
    /// to the database yet; the secret lives only in the encrypted hidden field of the confirmation form, and is written
    /// only when the first code is entered correctly within 10 minutes.
    pub async fn begin_two_factor_setup(
        &self,
        raw_sid: &str,
        current_password: &str,
    ) -> Result<TotpSetup> {
        let verified = self
            .check_current_password(raw_sid, current_password)
            .await?;
        self.begin_verified_two_factor_setup(verified).await
    }

    pub(crate) async fn begin_verified_two_factor_setup(
        &self,
        verified: VerifiedPassword<'_>,
    ) -> Result<TotpSetup> {
        self.config.two_factor.validate()?;
        let VerifiedPassword {
            claims, username, ..
        } = verified;
        // Checked again under the member row lock when the first code is entered.
        let email =
            crate::db::query("SELECT email, emailstatus FROM gonof_common_member WHERE uid = ?")
                .bind(claims.uid)
                .fetch_optional(self.db.executor())
                .await?
                .ok_or(IdentityError::Unauthorized)?;
        if email
            .try_get::<Option<String>, _>("email")?
            .is_some_and(|email| !email.is_empty())
            && email.try_get::<i64, _>("emailstatus")? != 1
        {
            return Err(super::external::verify_email_first());
        }
        let enabled: Option<i64> =
            crate::db::query_scalar("SELECT uid FROM gonof_common_member_two_factor WHERE uid = ?")
                .bind(claims.uid)
                .fetch_optional(self.db.executor())
                .await?;
        if enabled.is_some() {
            return Err(IdentityError::Conflict(
                crate::i18n::current().settings_error_two_factor_on(),
            ));
        }
        let mut secret = [0_u8; SECRET_BYTES];
        OsRng.fill_bytes(&mut secret);
        let token = self.config.cookie.seal_value(
            SETUP_AAD,
            &SetupClaims {
                uid: claims.uid,
                credentials_version: claims.credentials_version,
                secret: hex(&secret),
                expires_at: now() + SETUP_TTL,
            },
        )?;
        let issuer = crate::site::site_name(&*self.settings().await?);
        self.setup_view(&token, &username, &secret, &issuer)
    }

    /// When a wrong code is entered on the confirmation form, shows the QR code again with the same hidden token. Returns
    /// `None` when the token has expired.
    pub async fn resume_two_factor_setup(
        &self,
        member: &Member,
        token: &str,
    ) -> Option<Result<TotpSetup>> {
        let setup = self.setup_claims(token, member.uid, member.credentials_version, now())?;
        let secret = decode_hex(&setup.secret).filter(|bytes| bytes.len() == SECRET_BYTES)?;
        let issuer = match self.settings().await {
            Ok(settings) => crate::site::site_name(&settings),
            Err(error) => return Some(Err(error)),
        };
        Some(self.setup_view(token, &member.username, &secret, &issuer))
    }

    fn setup_claims(
        &self,
        token: &str,
        uid: i64,
        credentials_version: i64,
        now: i64,
    ) -> Option<SetupClaims> {
        self.config
            .cookie
            .open_value::<SetupClaims>(SETUP_AAD, token)
            .filter(|setup| {
                setup.uid == uid
                    && setup.credentials_version == credentials_version
                    && setup.expires_at > now
                    && setup.expires_at <= now + SETUP_TTL
            })
    }

    fn setup_view(
        &self,
        token: &str,
        username: &str,
        secret: &[u8],
        issuer: &str,
    ) -> Result<TotpSetup> {
        let encoded = base32(secret, false);
        let uri = otpauth_uri(issuer, username, &encoded);
        Ok(TotpSetup {
            token: token.to_owned(),
            secret: group(&encoded, 4, " "),
            qr_svg: qr_svg(&uri)?,
            uri,
        })
    }

    async fn insert_recovery_codes(
        &self,
        tx: &mut Tx<'_>,
        uid: i64,
        codes: &[String],
        timestamp: i64,
    ) -> Result<()> {
        let placeholders = vec!["(?, ?, ?)"; codes.len()].join(", ");
        let sql = format!(
            "INSERT INTO gonof_common_member_recovery_code (uid, code_hash, dateline) VALUES {placeholders}"
        );
        let mut insert = crate::db::query(&sql);
        for code in codes {
            insert = insert
                .bind(uid)
                .bind(recovery_hash(uid, code))
                .bind(timestamp);
        }
        insert.execute(&mut **tx).await?;
        Ok(())
    }

    fn seal_reveal(&self, uid: i64, codes: Vec<String>, timestamp: i64) -> Result<String> {
        Ok(self.config.cookie.seal_value(
            REVEAL_AAD,
            &RevealClaims {
                uid,
                codes,
                expires_at: timestamp + REVEAL_TTL,
            },
        )?)
    }

    /// Reads the display Cookie of the new recovery codes: accepts only the member's own, unexpired one; returns the
    /// display format.
    pub fn revealed_recovery_codes(&self, raw: &str, uid: i64) -> Option<Vec<String>> {
        let timestamp = now();
        self.config
            .cookie
            .open_value::<RevealClaims>(REVEAL_AAD, raw)
            .filter(|reveal| {
                reveal.uid == uid
                    && reveal.expires_at > timestamp
                    && reveal.expires_at <= timestamp + REVEAL_TTL
            })
            .map(|reveal| {
                reveal
                    .codes
                    .iter()
                    .map(|code| display_recovery_code(code))
                    .collect()
            })
    }

    /// Second step of enabling: the database is written only when the first code is correct, and recovery codes are
    /// generated at the same time; the credentials version +1 signs out other devices, and the current device stays
    /// signed in with the returned new session. API tokens and feed URLs are left alone.
    pub async fn enable_two_factor(
        &self,
        raw_sid: &str,
        token: &str,
        code: &str,
        metadata: &SessionMetadata,
    ) -> Result<TwoFactorEnabled> {
        self.config.two_factor.validate()?;
        let timestamp = now();
        let claims = self.cookie_claims(raw_sid)?;
        let setup = self
            .setup_claims(token, claims.uid, claims.credentials_version, timestamp)
            .ok_or_else(setup_expired)?;
        let secret = decode_hex(&setup.secret)
            .filter(|bytes| bytes.len() == SECRET_BYTES)
            .ok_or_else(setup_expired)?;
        let Answer::Totp(code) = answer(code) else {
            return Err(wrong_setup_code());
        };
        let step = accepted_step(&secret, &code, timestamp, 0).ok_or_else(wrong_setup_code)?;
        let ciphertext = seal_secret(&self.config.two_factor, claims.uid, &secret)?;
        let codes = new_recovery_codes();
        let mut tx = self.db.begin_write().await?;
        let member = self.security_member_locked(&mut tx, raw_sid).await?;
        if member.email_unverified() {
            return Err(super::external::verify_email_first());
        }
        if self.has_two_factor_locked(&mut tx, member.uid).await? {
            return Err(IdentityError::Conflict(
                crate::i18n::current().settings_error_two_factor_on(),
            ));
        }
        crate::db::query(
            "INSERT INTO gonof_common_member_two_factor (uid, totp_secret, totp_step, totp_enabled_at, enabled_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(member.uid)
        .bind(&ciphertext)
        .bind(step)
        .bind(timestamp)
        .bind(timestamp)
        .execute(&mut *tx)
        .await?;
        self.insert_recovery_codes(&mut tx, member.uid, &codes, timestamp)
            .await?;
        let version = member
            .credentials_version
            .checked_add(1)
            .ok_or(IdentityError::Conflict(
                crate::i18n::current().account_error_credentials_version(),
            ))?;
        let changed = crate::db::query("UPDATE gonof_common_member SET credentials_version = ? WHERE uid = ? AND credentials_version = ?")
            .bind(version)
            .bind(member.uid)
            .bind(member.credentials_version)
            .execute(self.db.observe(&mut *tx))
            .await?
            .rows_affected();
        if changed != 1 {
            return Err(IdentityError::Unauthorized);
        }
        let mut data = client_data(metadata);
        data["credentials_version"] = version.into();
        let log_id = self
            .audit(&mut tx, member.uid, ENABLE_LOG, data, timestamp)
            .await?;
        enqueue_notice(&self.db, &mut tx, &member, log_id, timestamp).await?;
        tx.commit().await?;
        let uid = member.uid;
        let mut member = member;
        member.credentials_version = version;
        let account = self.settings().await?.account.clone();
        Ok(TwoFactorEnabled {
            grant: self.grant_session(member, timestamp, &account)?,
            reveal: self.seal_reveal(uid, codes, timestamp)?,
        })
    }

    /// Regenerates recovery codes (requires the current password): the whole old batch is voided.
    pub async fn regenerate_recovery_codes(
        &self,
        raw_sid: &str,
        current_password: &str,
        metadata: &SessionMetadata,
    ) -> Result<String> {
        let verified = self
            .check_current_password(raw_sid, current_password)
            .await?;
        self.regenerate_recovery_codes_verified(verified, metadata)
            .await
    }

    pub(crate) async fn regenerate_recovery_codes_verified(
        &self,
        verified: VerifiedPassword<'_>,
        metadata: &SessionMetadata,
    ) -> Result<String> {
        let codes = new_recovery_codes();
        let timestamp = now();
        let mut tx = self.db.begin_write().await?;
        let member = self
            .password_unchanged_locked(&mut tx, verified.raw_sid, &verified.previous)
            .await?;
        if !self.has_two_factor_locked(&mut tx, member.uid).await? {
            return Err(IdentityError::Conflict(
                crate::i18n::current().settings_error_two_factor_off(),
            ));
        }
        crate::db::query("DELETE FROM gonof_common_member_recovery_code WHERE uid = ?")
            .bind(member.uid)
            .execute(self.db.observe(&mut *tx))
            .await?;
        self.insert_recovery_codes(&mut tx, member.uid, &codes, timestamp)
            .await?;
        let log_id = self
            .audit(
                &mut tx,
                member.uid,
                REGENERATE_LOG,
                client_data(metadata),
                timestamp,
            )
            .await?;
        enqueue_notice(&self.db, &mut tx, &member, log_id, timestamp).await?;
        tx.commit().await?;
        self.seal_reveal(member.uid, codes, timestamp)
    }

    /// Turns off two-factor authentication: requires the current password and passing the second step once more.
    /// Wrong input counts toward the failure count.
    pub async fn disable_two_factor(
        &self,
        raw_sid: &str,
        current_password: &str,
        code: &str,
        metadata: &SessionMetadata,
    ) -> Result<TwoFactorDisable> {
        let verified = self
            .check_current_password(raw_sid, current_password)
            .await?;
        self.disable_two_factor_verified(verified, code, metadata)
            .await
    }

    pub(crate) async fn disable_two_factor_verified(
        &self,
        verified: VerifiedPassword<'_>,
        code: &str,
        metadata: &SessionMetadata,
    ) -> Result<TwoFactorDisable> {
        let timestamp = now();
        let mut tx = self.db.begin_write().await?;
        let member = self
            .password_unchanged_locked(&mut tx, verified.raw_sid, &verified.previous)
            .await?;
        let Some(row) = self.factor_locked(&mut tx, member.uid).await? else {
            return Err(IdentityError::Conflict(
                crate::i18n::current().settings_error_two_factor_off(),
            ));
        };
        let outcome = match self
            .check_factor(&mut tx, &member, &row, code, metadata, timestamp)
            .await?
        {
            FactorCheck::Totp | FactorCheck::Recovery { .. } => {
                self.remove_two_factor(&mut tx, member.uid).await?;
                let log_id = self
                    .audit(
                        &mut tx,
                        member.uid,
                        DISABLE_LOG,
                        client_data(metadata),
                        timestamp,
                    )
                    .await?;
                enqueue_notice(&self.db, &mut tx, &member, log_id, timestamp).await?;
                TwoFactorDisable::Disabled
            }
            FactorCheck::Rejected => TwoFactorDisable::Rejected,
            FactorCheck::Locked(until) => TwoFactorDisable::Locked { until },
            FactorCheck::Unavailable => TwoFactorDisable::Unavailable,
        };
        tx.commit().await?;
        Ok(outcome)
    }

    async fn remove_two_factor(&self, tx: &mut Tx<'_>, uid: i64) -> Result<()> {
        for sql in [
            "DELETE FROM gonof_common_member_recovery_code WHERE uid = ?",
            "DELETE FROM gonof_common_member_two_factor WHERE uid = ?",
        ] {
            crate::db::query(sql)
                .bind(uid)
                .execute(self.db.observe(&mut **tx))
                .await?;
        }
        Ok(())
    }

    /// Password recovery claims an account whose email was never verified ([`Identity::reset_password`]): two-step
    /// verification turned on before then goes with it. Whoever turned it on knew the password and holds the
    /// authenticator, so keeping it would shut the address's owner out (and let the other person pass the second step
    /// after any other first one). This is the one case where recovery turns it off; for an account with a verified
    /// email recovery never does. Logged like turning it off, with the cause. The caller holds the member row lock.
    pub(super) async fn remove_two_factor_on_claim(
        &self,
        tx: &mut Tx<'_>,
        uid: i64,
        cause: &str,
        timestamp: i64,
    ) -> Result<()> {
        if !self.has_two_factor_locked(tx, uid).await? {
            return Ok(());
        }
        self.remove_two_factor(tx, uid).await?;
        self.audit(
            tx,
            uid,
            DISABLE_LOG,
            serde_json::json!({"cause": cause}),
            timestamp,
        )
        .await?;
        Ok(())
    }

    /// Resets the failure count on password change and password reset. The caller holds the member row lock.
    pub(super) async fn clear_two_factor_failures(&self, tx: &mut Tx<'_>, uid: i64) -> Result<()> {
        crate::db::query("UPDATE gonof_common_member_two_factor SET failed_attempts = 0, locked_until = 0 WHERE uid = ?")
            .bind(uid)
            .execute(self.db.observe(&mut **tx))
            .await?;
        Ok(())
    }

    /// Command-line reset: run on the server by the site owner after verifying identity offline. Deletes all
    /// two-factor data, bumps the credentials version by 1 (all devices signed out), revokes API tokens and feed URLs,
    /// writes an audit entry and sends a security alert. Returns `false` when the account has no two-factor
    /// authentication.
    pub async fn reset_two_factor(&self, uid: i64, reason: &str) -> Result<bool> {
        let reason = reason.trim();
        if reason.is_empty() || reason.chars().count() > 500 {
            return Err(IdentityError::Validation(
                "the reason must be 1 to 500 characters",
            ));
        }
        let timestamp = now();
        let mut tx = self.db.begin_write().await?;
        let member = self
            .member_locked(&mut tx, uid)
            .await
            .map_err(|error| match error {
                IdentityError::Unauthorized => IdentityError::NotFound,
                other => other,
            })?;
        if !self.has_two_factor_locked(&mut tx, uid).await? {
            tx.commit().await?;
            return Ok(false);
        }
        self.remove_two_factor(&mut tx, uid).await?;
        let version = member
            .credentials_version
            .checked_add(1)
            .ok_or(IdentityError::Conflict(
                "the credentials version is at its limit",
            ))?;
        crate::db::query("UPDATE gonof_common_member SET credentials_version = ? WHERE uid = ? AND credentials_version = ?")
            .bind(version)
            .bind(uid)
            .bind(member.credentials_version)
            .execute(self.db.observe(&mut *tx))
            .await?;
        let inserted = crate::db::query(
            "INSERT INTO gonof_common_log (uid, operationuid, type, data, source, dateline)
             VALUES (?, NULL, ?, ?, 'cli', ?)",
        )
        .bind(uid)
        .bind(RESET_LOG)
        .bind(serde_json::json!({"credentials_version": version, "reason": reason}).to_string())
        .bind(timestamp)
        .execute(self.db.observe(&mut *tx))
        .await?;
        let log_id = i64::try_from(inserted.last_insert_id()).context("log id out of range")?;
        self.revoke_api_keys_after(&mut tx, uid, RESET_LOG, timestamp)
            .await?;
        enqueue_notice(&self.db, &mut tx, &member, log_id, timestamp).await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Command-line re-encryption: re-encrypts authenticator secrets still on old keys with the current key. One
    /// conditional update per row, so sign-ins or turning off in the meantime cannot cause wrong writes. Old keys can be
    /// deleted only once everything is converted (`unreadable` empty too).
    pub async fn rekey_two_factor(&self) -> Result<RekeyReport> {
        self.config.two_factor.validate()?;
        let ring = self.config.two_factor.ring();
        let mut report = RekeyReport::default();
        let mut after = 0_i64;
        loop {
            let rows = crate::db::query(
                "SELECT uid, totp_secret FROM gonof_common_member_two_factor
                 WHERE uid > ? AND totp_secret IS NOT NULL ORDER BY uid LIMIT 100",
            )
            .bind(after)
            .fetch_all(self.db.executor())
            .await?;
            let Some(last) = rows.last() else {
                break;
            };
            after = last.try_get("uid")?;
            for row in &rows {
                let uid: i64 = row.try_get("uid")?;
                let raw = db::text(row, "totp_secret")?;
                if ring.uses_active_key(&raw) {
                    report.current += 1;
                    continue;
                }
                let Some(secret) = open_secret(&self.config.two_factor, uid, &raw) else {
                    report.unreadable.push(uid);
                    continue;
                };
                let resealed = seal_secret(&self.config.two_factor, uid, &secret)?;
                let changed = crate::db::query(
                    "UPDATE gonof_common_member_two_factor SET totp_secret = ? WHERE uid = ? AND totp_secret = ?",
                )
                .bind(&resealed)
                .bind(uid)
                .bind(&raw)
                .execute(&self.db.pool)
                .await?
                .rows_affected();
                if changed == 1 {
                    report.rekeyed += 1;
                }
            }
        }
        Ok(report)
    }
}

/// Tests only: computes the 6-digit code for a given moment from a secret (integration tests use it to simulate the
/// authenticator app).
pub fn totp_code_for_test(secret_base32: &str, timestamp: i64) -> Option<String> {
    let secret = decode_base32(secret_base32)?;
    Some(code_at(&secret, timestamp.div_euclid(STEP_SECONDS)))
}

fn decode_base32(value: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let (mut buffer, mut bits, mut out) = (0_u32, 0_u32, Vec::new());
    for byte in value
        .bytes()
        .filter(|b| !b.is_ascii_whitespace() && *b != b'=')
    {
        let index = ALPHABET
            .iter()
            .position(|c| *c == byte.to_ascii_uppercase())?;
        buffer = (buffer << 5) | u32::try_from(index).ok()?;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((buffer >> bits) & 0xff).ok()?);
        }
        buffer &= (1 << bits) - 1;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RFC_SECRET: &[u8] = b"12345678901234567890";

    #[test]
    fn hotp_matches_rfc_4226_appendix_d() {
        let expected = [
            755_224, 287_082, 359_152, 969_429, 338_314, 254_676, 287_922, 162_583, 399_871,
            520_489,
        ];
        for (counter, code) in expected.into_iter().enumerate() {
            assert_eq!(truncated(RFC_SECRET, counter as u64) % 1_000_000, code);
        }
    }

    #[test]
    fn totp_matches_rfc_6238_appendix_b_sha1_vectors() {
        for (time, code) in [
            (59_i64, 94_287_082_u32),
            (1_111_111_109, 7_081_804),
            (1_111_111_111, 14_050_471),
            (1_234_567_890, 89_005_924),
            (2_000_000_000, 69_279_037),
            (20_000_000_000, 65_353_130),
        ] {
            let step = u64::try_from(time / STEP_SECONDS).unwrap();
            assert_eq!(truncated(RFC_SECRET, step) % 100_000_000, code, "{time}");
            // The 6-digit code is the last 6 digits of the same truncated value.
            assert_eq!(
                code_at(RFC_SECRET, time / STEP_SECONDS),
                format!("{:06}", code % 1_000_000)
            );
        }
    }

    #[test]
    fn base32_matches_rfc_4648_section_10_and_round_trips() {
        for (input, expected) in [
            ("", ""),
            ("f", "MY======"),
            ("fo", "MZXQ===="),
            ("foo", "MZXW6==="),
            ("foob", "MZXW6YQ="),
            ("fooba", "MZXW6YTB"),
            ("foobar", "MZXW6YTBOI======"),
        ] {
            assert_eq!(base32(input.as_bytes(), true), expected);
            assert_eq!(decode_base32(expected).unwrap(), input.as_bytes());
        }
        assert_eq!(
            base32(RFC_SECRET, false),
            "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ"
        );
    }

    #[test]
    fn codes_are_accepted_within_one_step_and_never_twice() {
        let now = 1_111_111_111;
        let step = now / STEP_SECONDS;
        let current = code_at(RFC_SECRET, step);
        assert_eq!(accepted_step(RFC_SECRET, &current, now, 0), Some(step));
        // After use, the same code and any earlier code are no longer accepted (replay protection).
        assert_eq!(accepted_step(RFC_SECRET, &current, now, step), None);
        let previous = code_at(RFC_SECRET, step - 1);
        assert_eq!(accepted_step(RFC_SECRET, &previous, now, step), None);
        assert_eq!(accepted_step(RFC_SECRET, &previous, now, 0), Some(step - 1));
        // The next step's code can be used (phone clock slightly fast); two steps further on it has expired.
        let next = code_at(RFC_SECRET, step + 1);
        assert_eq!(accepted_step(RFC_SECRET, &next, now, step), Some(step + 1));
        let stale = code_at(RFC_SECRET, step - 2);
        let early = code_at(RFC_SECRET, step + 2);
        for code in [stale, early] {
            if code != current && code != previous && code != next {
                assert_eq!(accepted_step(RFC_SECRET, &code, now, 0), None);
            }
        }
    }

    #[test]
    fn answers_tell_codes_from_recovery_codes() {
        assert!(matches!(answer(" 123 456 "), Answer::Totp(code) if code == "123456"));
        assert!(matches!(
            answer("ABCD-efgh-2345-6789"),
            Answer::Recovery(code) if code == "abcdefgh23456789"
        ));
        for invalid in [
            "12345",
            "1234567",
            "abcd-efgh-2345-678",
            "0bcd-efgh-2345-6789",
            "",
        ] {
            assert!(matches!(answer(invalid), Answer::Invalid), "{invalid}");
        }
        assert!(matches!(answer(&"1".repeat(65)), Answer::Invalid));
    }

    #[test]
    fn recovery_codes_are_long_unique_and_hashed_per_member() {
        let codes = new_recovery_codes();
        assert_eq!(codes.len(), RECOVERY_CODE_COUNT);
        assert_eq!(
            codes.iter().collect::<std::collections::HashSet<_>>().len(),
            RECOVERY_CODE_COUNT
        );
        for code in &codes {
            assert_eq!(
                normalized_recovery_code(code).as_deref(),
                Some(code.as_str())
            );
            let shown = display_recovery_code(code);
            assert_eq!(shown.len(), 19);
            assert_eq!(
                normalized_recovery_code(&shown.to_uppercase()),
                Some(code.clone())
            );
        }
        assert_ne!(recovery_hash(1, &codes[0]), recovery_hash(2, &codes[0]));
        assert!(!recovery_hash(1, &codes[0]).contains(&codes[0]));
    }

    #[test]
    fn lockouts_grow_every_five_failures() {
        let locks: Vec<_> = (1..=21).map(lock_seconds).collect();
        assert_eq!(locks[3], 0);
        assert_eq!(locks[4], 900);
        assert_eq!(locks[5], 0);
        assert_eq!(locks[9], 3600);
        assert_eq!(locks[14], 86_400);
        assert_eq!(locks[19], 86_400);
        assert_eq!(locks.iter().filter(|seconds| **seconds > 0).count(), 4);
    }

    fn config() -> TwoFactorConfig {
        TwoFactorConfig {
            active_key: "33".repeat(32),
            previous_keys: vec![],
        }
    }

    #[test]
    fn secrets_are_bound_to_the_member_and_survive_rotation() {
        let old = config();
        let raw = seal_secret(&old, 7, RFC_SECRET).unwrap();
        assert!(!raw.contains(&hex(RFC_SECRET)));
        assert_eq!(open_secret(&old, 7, &raw).unwrap(), RFC_SECRET);
        // Moved as a whole row to another account, it cannot be decrypted.
        assert!(open_secret(&old, 8, &raw).is_none());
        let rotated = TwoFactorConfig {
            active_key: "44".repeat(32),
            previous_keys: vec![old.active_key.clone()],
        };
        assert_eq!(open_secret(&rotated, 7, &raw).unwrap(), RFC_SECRET);
        assert!(!rotated.ring().uses_active_key(&raw));
        let resealed = seal_secret(&rotated, 7, RFC_SECRET).unwrap();
        assert!(rotated.ring().uses_active_key(&resealed));
        assert!(open_secret(&old, 7, &resealed).is_none());
        // After removing the old key, ciphertexts not yet converted can no longer be opened.
        let dropped = TwoFactorConfig {
            active_key: rotated.active_key.clone(),
            previous_keys: vec![],
        };
        assert!(open_secret(&dropped, 7, &raw).is_none());
    }

    #[test]
    fn configuration_names_the_missing_key_and_redacts_values() {
        let error = TwoFactorConfig::default()
            .validate()
            .unwrap_err()
            .to_string();
        assert!(error.contains("identity.two_factor.active_key"), "{error}");
        assert!(error.contains("GONOF_TWO_FACTOR_KEY"), "{error}");
        let config = config();
        assert!(config.validate().is_ok());
        assert!(!format!("{config:?}").contains(&config.active_key));
        let duplicated = TwoFactorConfig {
            active_key: config.active_key.clone(),
            previous_keys: vec![config.active_key.to_uppercase()],
        };
        assert!(duplicated.validate().is_err());
    }

    #[test]
    fn setup_uri_and_qr_code_carry_the_secret_for_authenticator_apps() {
        let uri = otpauth_uri(
            "Gono Forum",
            "alice fan",
            "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ",
        );
        assert!(uri.starts_with("otpauth://totp/Gono%20Forum%3Aalice%20fan?"));
        assert!(uri.contains(
            "%3Aalice%20fan?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&issuer=Gono%20Forum&"
        ));
        assert!(uri.ends_with("&algorithm=SHA1&digits=6&period=30"));
        let svg = qr_svg(&uri).unwrap();
        assert!(svg.starts_with("<svg ") && svg.ends_with("</svg>"));
        assert!(svg.contains("<path d=\"M"));
        assert!(!svg.contains("GEZDGNBV"));
        assert_eq!(
            totp_code_for_test("GEZD GNBV GY3T QOJQ GEZD GNBV GY3T QOJQ", 59).as_deref(),
            Some("287082")
        );
    }
}
