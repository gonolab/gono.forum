//! Value rules for members, member tokens, API tokens, invite codes, rename records and role requests
//! (docs/architecture.md, "Data").
//!
//! The schema has no triggers, so code guarantees them:
//! - The values are defined here: account state [`AccountState`], token purpose [`TokenPurpose`], invite code
//!   status [`InviteStatus`], compared exactly (trailing spaces are not ignored as under PAD SPACE);
//! - The pre-write checks are the `check_*` functions here, which write paths call in the same transaction before
//!   running INSERT/UPDATE. The user-facing validation at the entry points (`normalize_username`,
//!   `normalize_email`, `validate_password`, etc.) comes first; the checks here only back up the stored-value
//!   rules, and a violation is a code defect that reports an internal error and rolls back;
//! - Forward-only states (account deletion is irreversible, a token is redeemed or revoked once, an invite code is
//!   used once) are expressed by conditional updates at each site: the `WHERE` includes the old state and the
//!   affected row count is checked, and `db::source_guards` pins down which files may change these columns.
//!
//! Length limits given by column widths (username 50, e-mail 255, password hash 512, bio 500, etc.) fail on
//! overflow under `STRICT_TRANS_TABLES` and are still guaranteed by the database; here we only check lower bounds,
//! combinations and values.

use crate::db::Tx;
use anyhow::{Result, ensure};

use crate::db::Database;

/// Account state. `active` is normal, `deleting` means account deletion is in progress (login is no longer
/// possible), `deleted` means deleted (irreversible).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountState {
    Active,
    Deleting,
    Deleted,
}

impl AccountState {
    pub const ALL: [Self; 3] = [Self::Active, Self::Deleting, Self::Deleted];

    /// The value stored in `gonof_common_member.account_state`.
    pub fn value(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Deleting => "deleting",
            Self::Deleted => "deleted",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.value() == value)
    }
}

/// Purpose of a member token; one e-mail link per purpose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenPurpose {
    EmailVerification,
    PasswordReset,
    EmailChange,
    AppealAccess,
    PrivacyAccess,
}

impl TokenPurpose {
    // SQL that filters by purpose with `purpose IN (...)` still writes literals; unit tests and source scans use this
    // to check that those literals are all valid purposes.
    #[cfg(test)]
    pub const ALL: [Self; 5] = [
        Self::EmailVerification,
        Self::PasswordReset,
        Self::EmailChange,
        Self::AppealAccess,
        Self::PrivacyAccess,
    ];

    /// The value stored in `gonof_common_member_token.purpose`.
    pub fn value(self) -> &'static str {
        match self {
            Self::EmailVerification => "email_verification",
            Self::PasswordReset => "password_reset",
            Self::EmailChange => "email_change",
            Self::AppealAccess => "appeal_access",
            Self::PrivacyAccess => "privacy_access",
        }
    }

    #[cfg(test)]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|purpose| purpose.value() == value)
    }
}

/// Invite code status. `used` means used for a registration (with a user and registration time); for the other
/// three, both are empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InviteStatus {
    Available,
    Used,
    Revoked,
    Frozen,
}

impl InviteStatus {
    pub const ALL: [Self; 4] = [Self::Available, Self::Used, Self::Revoked, Self::Frozen];

    /// The value stored in `gonof_common_invite.status`.
    pub fn value(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Used => "used",
            Self::Revoked => "revoked",
            Self::Frozen => "frozen",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|status| status.value() == value)
    }
}

fn trimmed(value: &str) -> bool {
    value.trim() == value
}

fn chars(value: &str) -> usize {
    value.chars().count()
}

/// Username (shared by the member table and rename records): 1–50 characters, no leading or trailing whitespace.
pub fn check_username(username: &str) -> Result<()> {
    ensure!(
        (1..=50).contains(&chars(username)) && trimmed(username),
        "username breaks the storage rules: {username:?}"
    );
    Ok(())
}

/// A member's e-mail: 3–255 characters, no leading or trailing whitespace.
pub fn check_email(email: &str) -> Result<()> {
    ensure!(
        (3..=255).contains(&chars(email)) && trimmed(email),
        "email breaks the storage rules"
    );
    Ok(())
}

/// A member's credential combination (the cross-column rules of the member row):
/// - Without a password, either a mobile number is bound (an SMS-registered account, which may have no e-mail), or
///   it is a synthetic author: no e-mail, no mobile number, unverified, ordinary member;
/// - With a password there must be an e-mail or a mobile number; only the site owner (adminid=1) may have neither;
/// - Verified requires an e-mail; admin role is only 0–5, verification status only 0 or 1, and the password hash
///   is not empty.
#[derive(Clone, Copy, Debug)]
pub struct Credentials<'a> {
    pub password: Option<&'a str>,
    pub email: Option<&'a str>,
    /// Bound mobile number (E.164).
    pub mobile: Option<&'a str>,
    /// Google or GitHub is bound (`gonof_common_member_external`). The binding table is not on the member row; the
    /// write entry point reads it while holding the member row lock.
    pub external: bool,
    pub emailstatus: i64,
    pub adminid: i64,
}

impl Credentials<'_> {
    pub fn check(&self) -> Result<()> {
        ensure!(
            matches!(self.emailstatus, 0 | 1) && (0..=5).contains(&self.adminid),
            "member verification state or role out of range"
        );
        if let Some(email) = self.email {
            check_email(email)?;
        }
        ensure!(
            self.password.is_none_or(|password| !password.is_empty()),
            "password hash must not be an empty string"
        );
        ensure!(
            self.emailstatus == 0 || self.email.is_some(),
            "a member without an email cannot be verified"
        );
        if let Some(mobile) = self.mobile {
            ensure!(
                crate::identity::phone::normalize_mobile(mobile)
                    .ok()
                    .as_deref()
                    == Some(mobile),
                "mobile number breaks the storage rules"
            );
        }
        let synthetic = self.email.is_none() && self.emailstatus == 0 && self.adminid == 0;
        // Ways to log in: password, mobile number (SMS), third-party binding. Without a password one of the latter
        // two is required, or it is a synthetic author (none at all); with a password there must also be a recovery
        // path: e-mail, mobile number, third-party binding, or being the site owner (reset from the command line).
        let shape = match self.password {
            None => {
                self.mobile.is_some()
                    || self.external
                    || (synthetic && self.mobile.is_none() && !self.external)
            }
            Some(_) => {
                self.email.is_some() || self.mobile.is_some() || self.external || self.adminid == 1
            }
        };
        ensure!(
            shape,
            "invalid combination of member password, email, mobile, linked accounts and role (without a password there must be a mobile, a linked account or a synthetic author; with a password there must be an email, a mobile, a linked account or the site owner)"
        );
        Ok(())
    }
}

/// Writes a member token: the only token INSERT in the whole repository (pinned by a source scan), with the values
/// checked before the write. Redemption and revocation are
/// expressed by conditional updates at each site (only unredeemed, unrevoked rows change, with a time no earlier
/// than issuance); see the individual write paths.
#[allow(clippy::too_many_arguments)]
pub async fn insert_token(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
    purpose: TokenPurpose,
    token_hash: &str,
    email: &str,
    dateline: i64,
    expires_at: i64,
) -> Result<()> {
    check_token(token_hash, email, dateline, expires_at)?;
    crate::db::query(
        "INSERT INTO gonof_common_member_token (uid, purpose, token_hash, email, dateline, expires_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(uid)
    .bind(purpose.value())
    .bind(token_hash)
    .bind(email)
    .bind(dateline)
    .bind(expires_at)
    .execute(db.observe(&mut **tx))
    .await?;
    Ok(())
}

fn check_token(token_hash: &str, email: &str, dateline: i64, expires_at: i64) -> Result<()> {
    ensure!(token_hash.len() == 64, "token hash is not 64 characters");
    check_email(email)?;
    ensure!(
        dateline >= 0 && expires_at > dateline,
        "invalid token issue and expiry times"
    );
    Ok(())
}

/// Writes an API token and returns its ID: the only API token INSERT in the whole repository (pinned by a source
/// scan), with the values checked before the write.
pub async fn insert_api_key(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
    key_hash: &str,
    key_hint: &str,
    dateline: i64,
    expires_at: i64,
) -> Result<i64> {
    check_api_key(key_hash, key_hint, dateline, expires_at)?;
    let id = crate::db::query(
        "INSERT INTO gonof_common_member_api_key (uid, key_hash, key_hint, dateline, expires_at)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(uid)
    .bind(key_hash)
    .bind(key_hint)
    .bind(dateline)
    .bind(expires_at)
    .execute(db.observe(&mut **tx))
    .await?
    .last_insert_id();
    Ok(i64::try_from(id)?)
}

/// Revokes all of the member's unrevoked API tokens and returns how many were revoked: the only statement in the
/// whole repository that writes `revoked_at` (pinned by a source scan). Only unrevoked rows change (revoked once);
/// the revocation time is no earlier than issuance (on clock rollback or clock skew across machines the issuance
/// time is used); when there is a revoker, the revocation time is always set.
pub async fn revoke_api_keys(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
    revoked_by: Option<i64>,
    timestamp: i64,
) -> Result<u64> {
    Ok(crate::db::query(
        "UPDATE gonof_common_member_api_key SET revoked_at = GREATEST(dateline, ?), revoked_by = ?
         WHERE uid = ? AND revoked_at IS NULL",
    )
    .bind(timestamp)
    .bind(revoked_by)
    .bind(uid)
    .execute(db.observe(&mut **tx))
    .await?
    .rows_affected())
}

/// A new API token row: 64-character hash, 1–16 character hint, expiry later than issuance.
fn check_api_key(key_hash: &str, key_hint: &str, dateline: i64, expires_at: i64) -> Result<()> {
    ensure!(
        key_hash.len() == 64 && (1..=16).contains(&chars(key_hint)),
        "API token hash or hint breaks the storage rules"
    );
    ensure!(
        dateline >= 0 && expires_at > dateline,
        "invalid API token issue and expiry times"
    );
    Ok(())
}

/// Writes an available invite code: the only invite code INSERT in the whole repository (pinned by a source scan),
/// with the values checked before the write. A new code is always
/// available, with no user and no registration time; afterwards its status changes only through conditional
/// updates (redeemed at registration, revoked by its owner, status changed in the admin area, account deletion),
/// and `used` is written in the same statement as the user and registration time.
pub async fn insert_invite(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
    code: &str,
    request_key: &str,
    dateline: i64,
    endtime: i64,
) -> Result<()> {
    check_invite(code, request_key, dateline, endtime)?;
    crate::db::query(
        "INSERT INTO gonof_common_invite (uid, code, request_key, dateline, endtime, status)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(uid)
    .bind(code)
    .bind(request_key)
    .bind(dateline)
    .bind(endtime)
    .bind(InviteStatus::Available.value())
    .execute(db.observe(&mut **tx))
    .await?;
    Ok(())
}

/// A new invite code row: code of 1–64 characters without leading or trailing whitespace, request key of 1–64
/// characters, expiry later than issuance.
fn check_invite(code: &str, request_key: &str, dateline: i64, endtime: i64) -> Result<()> {
    ensure!(
        (1..=64).contains(&chars(code)) && trimmed(code),
        "invite code breaks the storage rules"
    );
    ensure!(
        (1..=64).contains(&chars(request_key)),
        "invite request key breaks the storage rules"
    );
    ensure!(
        dateline >= 0 && endtime > dateline,
        "invalid invite issue and expiry times"
    );
    Ok(())
}

/// Keys and hashes of post requests and interaction requests: both tables are checked as 64 lowercase hex
/// characters. Every entry point already accepts only lowercase keys (`valid_request_key`) and every hash is a
/// `token_hash`, so the check changes no entry point's behavior.
fn check_request(uid: i64, request_key: &str, request_hash: &str, dateline: i64) -> Result<()> {
    ensure!(
        uid > 0
            && super::valid_request_key(request_key)
            && super::valid_request_key(request_hash)
            && dateline >= 0,
        "request key, request hash or time breaks the storage rules"
    );
    Ok(())
}

/// Records a post request (idempotency record for posting, replying, editing, likes, tips, activities and
/// moderation): the only INSERT into this table in the whole repository (pinned by a source scan).
pub async fn insert_post_request(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
    request_key: &str,
    request_hash: &str,
    pid: i64,
    dateline: i64,
) -> Result<()> {
    check_request(uid, request_key, request_hash, dateline)?;
    crate::db::query(
        "INSERT INTO gonof_forum_post_request(uid,request_key,request_hash,pid,dateline) VALUES(?,?,?,?,?)",
    )
    .bind(uid)
    .bind(request_key)
    .bind(request_hash)
    .bind(pid)
    .bind(dateline)
    .execute(db.observe(&mut **tx))
    .await?;
    Ok(())
}

/// Records an interaction request (idempotency record for follows, blocks, forum muting and invite code changes
/// in the admin area): the only INSERT into this table in the whole repository (pinned by a source scan).
pub async fn insert_interaction_request(
    db: &Database,
    tx: &mut Tx<'_>,
    uid: i64,
    request_key: &str,
    request_hash: &str,
    dateline: i64,
) -> Result<()> {
    check_request(uid, request_key, request_hash, dateline)?;
    crate::db::query(
        "INSERT INTO gonof_home_interaction_request(uid,request_key,request_hash,dateline) VALUES(?,?,?,?)",
    )
    .bind(uid)
    .bind(request_key)
    .bind(request_hash)
    .bind(dateline)
    .execute(db.observe(&mut **tx))
    .await?;
    Ok(())
}

/// Rename record: the old username is valid and the retention period ends after the rename time.
pub fn check_username_history(username: &str, dateline: i64, expires_at: i64) -> Result<()> {
    check_username(username)?;
    ensure!(
        dateline >= 0 && expires_at > dateline,
        "invalid retention period of the rename record"
    );
    Ok(())
}

/// Role request: request key and request hash are both 64 characters.
pub fn check_role_request(request_key: &str, request_hash: &str) -> Result<()> {
    ensure!(
        chars(request_key) == 64 && chars(request_hash) == 64,
        "role request key or request hash is not 64 characters"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enums_round_trip_exactly() {
        for state in AccountState::ALL {
            assert_eq!(AccountState::parse(state.value()), Some(state));
        }
        for purpose in TokenPurpose::ALL {
            assert_eq!(TokenPurpose::parse(purpose.value()), Some(purpose));
        }
        for status in InviteStatus::ALL {
            assert_eq!(InviteStatus::parse(status.value()), Some(status));
        }
        // Under PAD SPACE 'deleted ' equals 'deleted'; in code they are not equal.
        assert_eq!(AccountState::parse("deleted "), None);
        assert_eq!(TokenPurpose::parse("Password_reset"), None);
        assert_eq!(InviteStatus::parse("used "), None);
    }

    #[test]
    fn member_values_keep_the_rules_the_triggers_had() {
        assert!(check_username("alice").is_ok());
        // The limit counts characters, not bytes: 50 three-byte Chinese characters ("\u{8393}", "berry") pass,
        // 51 ("\u{6885}", "plum") do not.
        assert!(check_username(&"\u{8393}".repeat(50)).is_ok());
        for bad in ["", " alice", "alice ", "\talice", &"\u{6885}".repeat(51)] {
            assert!(check_username(bad).is_err(), "{bad:?}");
        }
        assert!(check_email("a@b").is_ok());
        for bad in ["a@", " a@b.c", "a@b.c "] {
            assert!(check_email(bad).is_err(), "{bad:?}");
        }
        let ok = |password, email, emailstatus, adminid| {
            Credentials {
                password,
                email,
                mobile: None,
                external: false,
                emailstatus,
                adminid,
            }
            .check()
            .is_ok()
        };
        let with_mobile = |password, email, mobile, adminid| {
            Credentials {
                password,
                email,
                mobile,
                external: false,
                emailstatus: 0,
                adminid,
            }
            .check()
            .is_ok()
        };
        let with_external = |password, email, emailstatus| {
            Credentials {
                password,
                email,
                mobile: None,
                external: true,
                emailstatus,
                adminid: 0,
            }
            .check()
            .is_ok()
        };
        // Third-party binding: an account registered through a third party has no password, and
        // its e-mail is either verified by the provider or absent; setting a password does not require an e-mail.
        assert!(with_external(None, Some("a@b.c"), 1));
        assert!(with_external(None, None, 0));
        assert!(with_external(Some("hash"), None, 0));
        assert!(
            !with_external(None, None, 1),
            "an account without e-mail cannot be verified"
        );
        // Mobile number: an SMS-registered account has no password and may have no e-mail; with a
        // password but no e-mail, a mobile number is also enough.
        assert!(with_mobile(None, None, Some("+8613800000000"), 0));
        assert!(with_mobile(None, Some("a@b.c"), Some("+8613800000000"), 0));
        assert!(with_mobile(Some("hash"), None, Some("+8613800000000"), 0));
        assert!(
            with_mobile(None, None, Some("+8613800000000"), 3),
            "an SMS account can be appointed staff"
        );
        assert!(
            !with_mobile(None, None, Some("13800000000"), 0),
            "mobile numbers are stored in E.164"
        );
        assert!(
            !with_mobile(None, None, Some("+12025550123"), 0),
            "only +86 is accepted"
        );
        // Synthetic author, ordinary member, unverified member, site owner without e-mail, administrator with e-mail.
        assert!(ok(None, None, 0, 0));
        assert!(ok(Some("hash"), Some("a@b.c"), 1, 0));
        assert!(ok(Some("hash"), Some("a@b.c"), 0, 0));
        assert!(ok(Some("hash"), None, 0, 1));
        assert!(ok(Some("hash"), Some("a@b.c"), 1, 3));
        // Combinations that must be refused.
        assert!(
            !ok(None, Some("a@b.c"), 0, 0),
            "a synthetic author has no e-mail"
        );
        assert!(!ok(None, None, 0, 1), "the site owner needs a password");
        assert!(
            !ok(Some("hash"), None, 0, 0),
            "an ordinary member with a password needs an e-mail"
        );
        assert!(
            !ok(Some("hash"), None, 0, 2),
            "only the site owner may have no e-mail"
        );
        assert!(
            !ok(Some("hash"), None, 1, 1),
            "an account without e-mail cannot be verified"
        );
        assert!(
            !ok(Some(""), Some("a@b.c"), 0, 0),
            "the password hash cannot be empty"
        );
        assert!(
            !ok(Some("hash"), Some("a@b.c"), 2, 0),
            "the verification state is only 0 or 1"
        );
        assert!(
            ok(Some("hash"), Some("a@b.c"), 1, 5),
            "trainee moderator 4, reviewer 5"
        );
        assert!(
            !ok(Some("hash"), Some("a@b.c"), 1, 6),
            "the staff role is only 0-5"
        );
        assert!(
            !ok(Some("hash"), Some("a@b.c"), 1, -1),
            "the staff role is only 0-5"
        );
    }

    #[test]
    fn token_key_invite_and_history_values_keep_the_rules_the_triggers_had() {
        let hash = "a".repeat(64);
        assert!(check_token(&hash, "a@b.c", 100, 101).is_ok());
        assert!(check_token("short", "a@b.c", 100, 101).is_err());
        assert!(check_token(&hash, "ab", 100, 101).is_err());
        assert!(check_token(&hash, "a@b.c", 100, 100).is_err());
        assert!(check_token(&hash, "a@b.c", -1, 100).is_err());

        assert!(check_api_key(&hash, "gonof_abcd", 100, 101).is_ok());
        assert!(check_api_key(&hash, "", 100, 101).is_err());
        assert!(check_api_key(&hash, &"x".repeat(17), 100, 101).is_err());
        assert!(check_api_key(&hash[1..], "gonof_abcd", 100, 101).is_err());
        assert!(check_api_key(&hash, "gonof_abcd", 100, 99).is_err());

        assert!(check_invite("CaseSensitive", "first", 100, 200).is_ok());
        assert!(check_invite("", "first", 100, 200).is_err());
        assert!(check_invite(" code", "first", 100, 200).is_err());
        assert!(check_invite("code", "", 100, 200).is_err());
        assert!(check_invite("code", "first", 200, 200).is_err());

        assert!(check_username_history("old-name", 100, 101).is_ok());
        assert!(check_username_history("old-name ", 100, 101).is_err());
        assert!(check_username_history("old-name", 100, 100).is_err());

        assert!(check_role_request(&hash, &hash).is_ok());
        assert!(check_role_request(&hash, "short").is_err());
    }
}
