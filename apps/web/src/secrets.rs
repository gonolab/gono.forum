//! Private config `[common] encryption_key`: encrypts the external service secrets entered in the admin area and stored
//! in `gonof_common_settings`: the mail SMTP authorization code (`mail.transport`) and the SMS provider's AccessKey
//! Secret and the Google and GitHub Client Secrets (`login.providers`); mail and SMS share this one key.
//!
//! - AES-256-GCM, with a random 12-byte nonce placed before the ciphertext; the additional data is supplied by each use
//!   (binding the config row with the service address and account), so ciphertext moved elsewhere by editing the
//!   database cannot be decrypted.
//! - There is no rotation: after changing this key, the authorization code and AccessKey Secret must be re-entered in
//!   the admin area. That is why it is kept separate from the cookie and two-factor keys, and startup checks that they
//!   all differ ([`crate::config::Config::validate`]).
//! - When empty, only local delivery works (mail goes to `.local/mail/`, SMS to `.local/sms/`), and configuration that
//!   needs encryption cannot be saved.
use std::fmt;

use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use rand::{RngCore, rngs::OsRng};
use serde::Deserialize;

/// The message shown to administrators when the key is missing or malformed, in this request's language.
pub fn missing_key() -> &'static str {
    crate::i18n::current().setup_config_encryption_key()
}

#[derive(Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CommonConfig {
    pub encryption_key: String,
}

impl fmt::Debug for CommonConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommonConfig")
            .field("encryption_key", &"[redacted]")
            .finish()
    }
}

impl CommonConfig {
    /// Empty is fine; if set it must be 64 hex characters.
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.encryption_key.is_empty() || self.key().is_some(),
            "{}",
            missing_key()
        );
        Ok(())
    }

    /// Whether a usable key is configured.
    pub fn configured(&self) -> bool {
        self.key().is_some()
    }

    fn key(&self) -> Option<[u8; 32]> {
        let text = self.encryption_key.as_bytes();
        if text.len() != 64 || !text.iter().all(u8::is_ascii_hexdigit) {
            return None;
        }
        let mut key = [0; 32];
        for (i, value) in key.iter_mut().enumerate() {
            *value = u8::from_str_radix(&self.encryption_key[i * 2..i * 2 + 2], 16).ok()?;
        }
        Some(key)
    }

    /// Encrypts: nonce first, ciphertext after. `None` when no key is configured.
    pub fn seal(&self, plain: &str, aad: &[u8]) -> Option<Vec<u8>> {
        let cipher = Aes256Gcm::new_from_slice(&self.key()?).ok()?;
        let mut nonce = [0; 12];
        OsRng.fill_bytes(&mut nonce);
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plain.as_bytes(),
                    aad,
                },
            )
            .ok()?;
        Some([nonce.to_vec(), ciphertext].concat())
    }

    /// Decrypts; `None` when no key is configured, the key is wrong, the additional data does not match, or the data
    /// was tampered with.
    pub fn open(&self, encrypted: &[u8], aad: &[u8]) -> Option<String> {
        if encrypted.len() < 28 {
            return None;
        }
        let cipher = Aes256Gcm::new_from_slice(&self.key()?).ok()?;
        let bytes = cipher
            .decrypt(
                Nonce::from_slice(&encrypted[..12]),
                Payload {
                    msg: &encrypted[12..],
                    aad,
                },
            )
            .ok()?;
        String::from_utf8(bytes).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealing_needs_the_same_key_and_context() {
        let config = CommonConfig {
            encryption_key: "01".repeat(32),
        };
        let sealed = config.seal("authorization-secret", b"context").unwrap();
        assert_ne!(
            sealed,
            config.seal("authorization-secret", b"context").unwrap()
        );
        assert_eq!(
            config.open(&sealed, b"context").as_deref(),
            Some("authorization-secret")
        );
        assert!(config.open(&sealed, b"elsewhere").is_none());
        let other = CommonConfig {
            encryption_key: "02".repeat(32),
        };
        assert!(other.open(&sealed, b"context").is_none());
        let mut tampered = sealed.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(config.open(&tampered, b"context").is_none());
        assert!(CommonConfig::default().seal("x", b"context").is_none());
        assert!(!CommonConfig::default().configured());
        assert!(CommonConfig::default().validate().is_ok());
        assert!(
            CommonConfig {
                encryption_key: "zz".repeat(32)
            }
            .validate()
            .is_err()
        );
        assert!(!format!("{config:?}").contains(&config.encryption_key));
    }
}
