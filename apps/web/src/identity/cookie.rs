//! Login credentials use a separate stable key; the online table takes no part in credential verification.
use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_COOKIE_LENGTH: usize = 2048;
const AAD: &[u8] = b"gonof.auth-cookie.v1";

#[derive(Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CookieConfig {
    pub active_key: String,
    pub previous_keys: Vec<String>,
}

impl std::fmt::Debug for CookieConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CookieConfig")
            .field("active_key", &"[redacted]")
            .field("previous_keys", &"[redacted]")
            .finish()
    }
}

fn decode_key(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut bytes = [0; 32];
    for (byte, pair) in bytes.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(bytes)
}

/// A 32-byte key in 64 hexadecimal digits. Two-factor keys use the same format as the cookie key.
pub(super) fn valid_key(value: &str) -> bool {
    decode_key(value).is_some()
}

fn key_id(key: &[u8; 32]) -> String {
    super::hex(&Sha256::digest(key)[..8])
}

impl CookieConfig {
    pub fn validate(&self) -> Result<()> {
        let active = decode_key(&self.active_key).ok_or_else(|| anyhow::anyhow!(
            "identity.cookie.active_key is missing or invalid: set a stable random 64-digit hex key in the private configuration or GONOF_AUTH_COOKIE_KEY"
        ))?;
        ensure!(
            self.previous_keys.len() <= 3,
            "at most 3 previous cookie keys are kept"
        );
        let mut seen = std::collections::HashSet::from([key_id(&active)]);
        for key in &self.previous_keys {
            let key = decode_key(key)
                .ok_or_else(|| anyhow::anyhow!("previous cookie keys must be 64 hex digits"))?;
            ensure!(seen.insert(key_id(&key)), "cookie keys must not repeat");
        }
        Ok(())
    }

    pub(super) fn seal(&self, claims: &CookieClaims) -> Result<String> {
        self.seal_value(AAD, claims)
    }

    pub(super) fn open(&self, raw: &str) -> Option<CookieClaims> {
        self.open_value(AAD, raw)
    }

    /// Encrypts another payload with the same key. `aad` distinguishes the purpose: ciphertext of one kind of payload
    /// cannot be decrypted as another.
    pub(super) fn seal_value<T: Serialize>(&self, aad: &[u8], value: &T) -> Result<String> {
        self.validate()?;
        let key = decode_key(&self.active_key).expect("validated cookie key");
        let cipher =
            Aes256Gcm::new_from_slice(&key).map_err(|_| anyhow::anyhow!("invalid cookie key"))?;
        let mut nonce = [0_u8; 12];
        OsRng.fill_bytes(&mut nonce);
        let plaintext = serde_json::to_vec(value)?;
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &plaintext,
                    aad,
                },
            )
            .map_err(|_| anyhow::anyhow!("failed to encrypt the login credential"))?;
        let mut payload = nonce.to_vec();
        payload.extend_from_slice(&ciphertext);
        Ok(format!(
            "v1.{}.{}",
            key_id(&key),
            URL_SAFE_NO_PAD.encode(payload)
        ))
    }

    pub(super) fn open_value<T: serde::de::DeserializeOwned>(
        &self,
        aad: &[u8],
        raw: &str,
    ) -> Option<T> {
        if raw.is_empty() || raw.len() > MAX_COOKIE_LENGTH {
            return None;
        }
        let mut parts = raw.split('.');
        if parts.next()? != "v1" {
            return None;
        }
        let id = parts.next()?;
        let payload = parts.next()?;
        if parts.next().is_some() || id.len() != 16 {
            return None;
        }
        let key = std::iter::once(&self.active_key)
            .chain(&self.previous_keys)
            .filter_map(|key| decode_key(key))
            .find(|key| key_id(key) == id)?;
        let payload = URL_SAFE_NO_PAD.decode(payload).ok()?;
        if payload.len() < 12 + 16 {
            return None;
        }
        let cipher = Aes256Gcm::new_from_slice(&key).ok()?;
        let plaintext = cipher
            .decrypt(
                Nonce::from_slice(&payload[..12]),
                Payload {
                    msg: &payload[12..],
                    aad,
                },
            )
            .ok()?;
        serde_json::from_slice(&plaintext).ok()
    }

    /// Keys for another purpose, derived from the cookie keys, current first: HMAC-SHA256 of `label` under each key.
    /// A derived key reveals neither the cookie key nor the keys derived for other labels, and it rotates with the
    /// cookie key (`previous_keys` give the previous derived keys).
    pub(super) fn derived_keys(&self, label: &[u8]) -> Result<Vec<[u8; 32]>> {
        self.validate()?;
        Ok(std::iter::once(&self.active_key)
            .chain(&self.previous_keys)
            .map(|key| {
                let key = decode_key(key).expect("validated cookie key");
                let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&key)
                    .expect("HMAC accepts any key length");
                mac.update(label);
                let mut derived = [0; 32];
                derived.copy_from_slice(&mac.finalize().into_bytes());
                derived
            })
            .collect())
    }

    pub(super) fn uses_active_key(&self, raw: &str) -> bool {
        decode_key(&self.active_key)
            .is_some_and(|key| raw.split('.').nth(1) == Some(key_id(&key).as_str()))
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CookieClaims {
    pub uid: i64,
    pub credentials_version: i64,
    pub login_id: String,
    pub csrf_token: String,
    pub issued_at: i64,
    pub idle_expires_at: i64,
    pub absolute_expires_at: i64,
}

impl CookieClaims {
    pub(super) fn valid_at(&self, timestamp: i64) -> bool {
        let token = |value: &str| value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit());
        self.uid > 0
            && self.credentials_version > 0
            && token(&self.login_id)
            && token(&self.csrf_token)
            && self.issued_at >= 0
            && self.issued_at <= timestamp
            && timestamp < self.idle_expires_at
            && self.idle_expires_at <= self.absolute_expires_at
            && self.absolute_expires_at.saturating_sub(self.issued_at) <= 31_536_000
    }

    pub(super) fn renew_at(&mut self, timestamp: i64, idle_seconds: i64) -> bool {
        if !self.valid_at(timestamp) || idle_seconds <= 0 {
            return false;
        }
        let desired = timestamp
            .saturating_add(idle_seconds)
            .min(self.absolute_expires_at);
        let previous_activity = self.idle_expires_at.saturating_sub(idle_seconds);
        let interval = 60.min((idle_seconds / 2).max(1));
        if desired > self.idle_expires_at && timestamp.saturating_sub(previous_activity) >= interval
        {
            self.idle_expires_at = desired;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> CookieConfig {
        CookieConfig {
            active_key: super::super::random_token(),
            previous_keys: vec![],
        }
    }
    fn claims() -> CookieClaims {
        CookieClaims {
            uid: 7,
            credentials_version: 3,
            login_id: super::super::random_token(),
            csrf_token: super::super::random_token(),
            issued_at: 100,
            idle_expires_at: 200,
            absolute_expires_at: 300,
        }
    }

    #[test]
    fn authenticated_cookie_rejects_tampering_unknown_keys_and_malformed_payloads() {
        let config = config();
        let original = claims();
        let raw = config.seal(&original).unwrap();
        assert_ne!(raw, config.seal(&original).unwrap());
        assert_eq!(config.open(&raw).unwrap().csrf_token, original.csrf_token);
        assert!(config.open(&raw).unwrap().valid_at(199));
        assert!(!config.open(&raw).unwrap().valid_at(200));
        assert!(!config.open(&raw).unwrap().valid_at(99));
        let mut corrupted = raw.clone().into_bytes();
        let last = corrupted.len() - 8;
        corrupted[last] = if corrupted[last] == b'A' { b'B' } else { b'A' };
        assert!(
            config
                .open(std::str::from_utf8(&corrupted).unwrap())
                .is_none()
        );
        let other_key = CookieConfig {
            active_key: super::super::random_token(),
            previous_keys: vec![],
        };
        assert!(other_key.open(&raw).is_none());
        for invalid in ["", "v2.abc.def", "v1.a.b.c", &"x".repeat(2049)] {
            assert!(config.open(invalid).is_none());
        }
    }

    #[test]
    fn rotation_only_uses_active_key_for_new_cookies_and_redacts_configuration() {
        let old = config();
        let mut rotated = config();
        rotated.previous_keys.push(old.active_key.clone());
        let raw = old.seal(&claims()).unwrap();
        assert!(rotated.open(&raw).is_some());
        assert!(!rotated.uses_active_key(&raw));
        let current = rotated.seal(&rotated.open(&raw).unwrap()).unwrap();
        assert!(rotated.uses_active_key(&current));
        assert!(old.open(&current).is_none());
        assert!(!format!("{rotated:?}").contains(&rotated.active_key));
        assert!(!format!("{rotated:?}").contains(&old.active_key));
        rotated.previous_keys.push(rotated.active_key.clone());
        assert!(rotated.validate().is_err());
        assert!(CookieConfig::default().validate().is_err());
    }

    #[test]
    fn renewal_preserves_login_version_csrf_and_absolute_deadline() {
        let mut claims = claims();
        let original = claims.clone();
        assert!(!claims.renew_at(149, 100));
        assert!(claims.renew_at(150, 100));
        assert_eq!(claims.idle_expires_at, 250);
        assert!(claims.renew_at(240, 100));
        assert_eq!(claims.idle_expires_at, 300);
        assert!(!claims.renew_at(299, 100));
        assert!(!claims.valid_at(300));
        assert!(!claims.renew_at(300, 100));
        assert_eq!(claims.credentials_version, original.credentials_version);
        assert_eq!(claims.issued_at, original.issued_at);
        assert_eq!(claims.absolute_expires_at, original.absolute_expires_at);
        assert_eq!(claims.login_id, original.login_id);
        assert_eq!(claims.csrf_token, original.csrf_token);
        let mut expired = original;
        assert!(!expired.renew_at(200, 100));
        assert_eq!(expired.idle_expires_at, 200);
    }

    #[test]
    fn payload_rejects_invalid_identity_and_deadline_ordering() {
        let original = claims();
        let mut invalid = original.clone();
        invalid.credentials_version = 0;
        assert!(!invalid.valid_at(150));
        invalid = original.clone();
        invalid.uid = 0;
        assert!(!invalid.valid_at(150));
        invalid = original.clone();
        invalid.absolute_expires_at = 199;
        assert!(!invalid.valid_at(150));
        invalid = original.clone();
        invalid.absolute_expires_at = invalid.issued_at + 31_536_001;
        assert!(!invalid.valid_at(150));
        invalid = original;
        invalid.csrf_token.clear();
        assert!(!invalid.valid_at(150));
    }
}
