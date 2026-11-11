//! New-device sign-in alerts.
//!
//! A "new device" is a browser that does not carry the device marker saying "this account has signed in on this
//! machine". The marker is an encrypted Cookie holding at most [`MARKER_CAPACITY`] uids that have signed in or
//! registered successfully on this machine; it is not a credential, it only reduces alerts. Signing out does not clear
//! it, and changing the password or signing out of all devices does not invalidate it either.
//!
//! Alerts go only to verified email addresses, at most [`HOURLY_LIMIT`] per account per hour. The outbox payload stores
//! only the row ID of the sign-in log; IP and browser information are read from that `identity.login` log at delivery
//! time and not stored a second time.
use crate::db::Tx;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{Member, cookie::CookieConfig};
use crate::{db::Database, notifications::preferences};

pub(crate) const EVENT_TYPE: &str = "login_alert";
pub(crate) const HOURLY_LIMIT: i64 = 5;
/// Lifetime of the device marker Cookie: 400 days, the maximum Chrome allows.
pub const MARKER_MAX_AGE: i64 = 400 * 86_400;
const MARKER_CAPACITY: usize = 8;
const MARKER_AAD: &[u8] = b"gonof.device-marker.v1";

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    uids: Vec<i64>,
}

/// Whether the marker contains this account. A marker that cannot be opened (forged, all keys changed, wrong format)
/// counts as not containing it.
pub(super) fn known(config: &CookieConfig, raw: &str, uid: i64) -> bool {
    config
        .open_value::<Marker>(MARKER_AAD, raw)
        .is_some_and(|marker| marker.uids.contains(&uid))
}

/// Adds the account to the marker and returns the new ciphertext. The most recent sign-in goes last; when capacity is
/// exceeded the oldest is dropped.
pub(super) fn remember(config: &CookieConfig, raw: &str, uid: i64) -> Result<String> {
    let mut marker = config
        .open_value::<Marker>(MARKER_AAD, raw)
        .unwrap_or_default();
    marker.uids.retain(|known| *known != uid && *known > 0);
    marker.uids.push(uid);
    let overflow = marker.uids.len().saturating_sub(MARKER_CAPACITY);
    marker.uids.drain(..overflow);
    config.seal_value(MARKER_AAD, &marker)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LoginAlertMail {
    pub uid: i64,
    /// Row ID of the `identity.login` log written by this sign-in.
    pub log_id: i64,
    /// Digest of the recipient email address at enqueue time; delivery is abandoned if the email has changed.
    pub email_hash: String,
}

pub(crate) fn key_range(uid: i64) -> (String, String) {
    (
        format!("{EVENT_TYPE}:{uid}:"),
        format!("{EVENT_TYPE}:{uid};"),
    )
}

/// The caller holds the member row lock, and this sign-in has already written the `log_id` sign-in log.
pub(super) async fn enqueue(
    db: &Database,
    tx: &mut Tx<'_>,
    member: &Member,
    log_id: i64,
    now: i64,
) -> Result<()> {
    let Some(email) = member.email.as_deref().filter(|email| !email.is_empty()) else {
        return Ok(());
    };
    if !member.email_verified || !preferences::locked(db, tx, member.uid).await?.email_login {
        return Ok(());
    }
    let (low, high) = key_range(member.uid);
    let recent: i64 = crate::db::query_scalar(
        "SELECT COUNT(*) FROM gonof_common_outbox WHERE event_key>=? AND event_key<? AND dateline>=?",
    )
    .bind(&low)
    .bind(&high)
    .bind(now.saturating_sub(3600))
    .fetch_one(db.observe(&mut **tx))
    .await?;
    if recent >= HOURLY_LIMIT {
        return Ok(());
    }
    let payload = serde_json::to_string(&LoginAlertMail {
        uid: member.uid,
        log_id,
        email_hash: super::token_hash(email),
    })
    .context("failed to build the login alert event")?;
    crate::db::query("INSERT INTO gonof_common_outbox(event_key,event_type,aggregate_type,aggregate_id,payload,dateline,available_at) VALUES(?,?,'member',?,?,?,?)")
        .bind(format!("{low}{log_id}"))
        .bind(EVENT_TYPE)
        .bind(member.uid)
        .bind(payload)
        .bind(now)
        .bind(now)
        .execute(db.observe(&mut **tx))
        .await?;
    Ok(())
}

/// Roughly classifies the User-Agent as "browser · system", without keeping the raw text. Unrecognized
/// parts are written as "Unknown browser" or "Unknown system" ("Unknown device" when neither is recognized).
pub fn device_summary(lang: crate::i18n::Lang, user_agent: &str) -> String {
    let has = |needle: &str| user_agent.contains(needle);
    let browser = if has("Edg/") || has("EdgA/") || has("EdgiOS/") {
        Some("Edge")
    } else if has("OPR/") || has("Opera") {
        Some("Opera")
    } else if has("SamsungBrowser/") {
        Some("Samsung Internet")
    } else if has("Firefox/") || has("FxiOS/") {
        Some("Firefox")
    } else if has("Chrome/") || has("CriOS/") || has("Chromium/") {
        Some("Chrome")
    } else if has("Safari/") && has("Version/") {
        Some("Safari")
    } else {
        None
    };
    let system = if has("Windows") {
        Some("Windows")
    } else if has("iPhone") || has("iPad") || has("iPod") {
        Some("iOS")
    } else if has("Android") {
        Some("Android")
    } else if has("CrOS") {
        Some("ChromeOS")
    } else if has("Mac OS X") || has("Macintosh") {
        Some("macOS")
    } else if has("Linux") {
        Some("Linux")
    } else {
        None
    };
    match (browser, system) {
        (None, None) => lang.mail_unknown_device().into(),
        (browser, system) => format!(
            "{} · {}",
            browser.unwrap_or(lang.mail_unknown_browser()),
            system.unwrap_or(lang.mail_unknown_system())
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> CookieConfig {
        CookieConfig {
            active_key: "11".repeat(32),
            previous_keys: vec![],
        }
    }

    #[test]
    fn marker_remembers_recent_accounts_and_rejects_foreign_ciphertext() {
        let config = config();
        assert!(!known(&config, "", 7));
        let mut raw = String::new();
        for uid in 1..=10 {
            raw = remember(&config, &raw, uid).unwrap();
        }
        // Capacity 8: the oldest, 1 and 2, are pushed out.
        assert!(!known(&config, &raw, 1) && !known(&config, &raw, 2));
        assert!((3..=10).all(|uid| known(&config, &raw, uid)));
        // Sign in with 3 again: it moves to the end, and when another new account is added the one pushed out is 4.
        raw = remember(&config, &raw, 3).unwrap();
        raw = remember(&config, &raw, 11).unwrap();
        assert!(known(&config, &raw, 3) && !known(&config, &raw, 4));
        // A different key cannot open it, so it is treated as a new device; the session Cookie's ciphertext cannot
        // pose as a device marker either.
        let other = CookieConfig {
            active_key: "22".repeat(32),
            previous_keys: vec![],
        };
        assert!(!known(&other, &raw, 3));
        assert!(!known(&config, "v1.0000000000000000.AAAA", 3));
    }

    #[test]
    fn device_summary_names_browser_and_system_only() {
        for (agent, expected) in [
            (
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36",
                "Chrome · macOS",
            ),
            (
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36 Edg/141.0.0.0",
                "Edge · Windows",
            ),
            (
                "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1",
                "Safari · iOS",
            ),
            (
                "Mozilla/5.0 (Linux; Android 15) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Mobile Safari/537.36",
                "Chrome · Android",
            ),
            (
                "Mozilla/5.0 (X11; Linux x86_64; rv:140.0) Gecko/20100101 Firefox/140.0",
                "Firefox · Linux",
            ),
            ("curl/8.7.1", "Unknown device"),
            ("", "Unknown device"),
        ] {
            assert_eq!(
                device_summary(crate::i18n::Lang::EN, agent),
                expected,
                "{agent}"
            );
        }
    }
}
