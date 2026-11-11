//! Token plaintext shown only once: personal feed URLs and member API tokens.
//!
//! The POST that generates it does not render the plaintext directly; it puts the plaintext into an encrypted cookie
//! sent only to the settings page and valid for [`SECRET_REVEAL_TTL`] seconds, then redirects 303 to GET the settings
//! page, which clears it once read. A refresh then shows the GET page, does not resubmit the generation form (a refresh
//! used to quietly generate another one, invalidating the URL or token just copied), and no longer shows the
//! plaintext. Same approach as the two-factor recovery code page; the ciphertext is encrypted with the cookie key, and
//! the AAD is per purpose, so ciphertext of one purpose cannot be decrypted for another.
use super::*;

/// Lifetime of the reveal cookie: the redirect is followed almost immediately; the 10 minutes are only for slow
/// networks and manual following.
pub const SECRET_REVEAL_TTL: i64 = 600;

/// Which kind of token the reveal cookie is for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecretReveal {
    FeedToken,
    ApiKey,
}

impl SecretReveal {
    fn aad(self) -> &'static [u8] {
        match self {
            Self::FeedToken => b"gonof.feed-token-reveal.v1",
            Self::ApiKey => b"gonof.api-key-reveal.v1",
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SecretRevealClaims {
    uid: i64,
    secret: String,
    expires_at: i64,
}

fn seal(
    cookie: &CookieConfig,
    kind: SecretReveal,
    uid: i64,
    secret: &str,
    timestamp: i64,
) -> anyhow::Result<String> {
    cookie.seal_value(
        kind.aad(),
        &SecretRevealClaims {
            uid,
            secret: secret.to_owned(),
            expires_at: timestamp + SECRET_REVEAL_TTL,
        },
    )
}

fn open(
    cookie: &CookieConfig,
    kind: SecretReveal,
    raw: &str,
    uid: i64,
    timestamp: i64,
) -> Option<String> {
    cookie
        .open_value::<SecretRevealClaims>(kind.aad(), raw)
        .filter(|claims| {
            claims.uid == uid
                && claims.expires_at > timestamp
                && claims.expires_at <= timestamp + SECRET_REVEAL_TTL
        })
        .map(|claims| claims.secret)
}

impl Identity {
    /// Seals the plaintext of the newly generated token into the reveal cookie value.
    pub fn seal_secret_reveal(
        &self,
        kind: SecretReveal,
        uid: i64,
        secret: &str,
    ) -> anyhow::Result<String> {
        seal(&self.config.cookie, kind, uid, secret, now())
    }

    /// Reads the reveal cookie: only accepts one that belongs to the viewer, has not expired and has the same purpose.
    /// The caller must still check that it is still the current token (its first few characters match the hint in the
    /// database); tokens that were regenerated, disabled or revoked are no longer shown.
    pub fn revealed_secret(&self, kind: SecretReveal, raw: &str, uid: i64) -> Option<String> {
        open(&self.config.cookie, kind, raw, uid, now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cookie() -> CookieConfig {
        CookieConfig {
            active_key: "11".repeat(32),
            previous_keys: Vec::new(),
        }
    }

    #[test]
    fn reveal_opens_only_for_its_owner_purpose_and_lifetime() {
        let cookie = cookie();
        let sealed = seal(&cookie, SecretReveal::FeedToken, 7, "abcdef0123", 1_000).unwrap();
        assert!(
            !sealed.contains("abcdef0123"),
            "the cookie does not hold the plain text"
        );
        assert_eq!(
            open(&cookie, SecretReveal::FeedToken, &sealed, 7, 1_000),
            Some("abcdef0123".into())
        );
        assert_eq!(
            open(&cookie, SecretReveal::FeedToken, &sealed, 8, 1_000),
            None,
            "another member cannot open it"
        );
        assert_eq!(
            open(&cookie, SecretReveal::ApiKey, &sealed, 7, 1_000),
            None,
            "a sealed feed token does not open as an API key"
        );
        assert_eq!(
            open(
                &cookie,
                SecretReveal::FeedToken,
                &sealed,
                7,
                1_000 + SECRET_REVEAL_TTL
            ),
            None,
            "it is no longer shown once expired"
        );
        assert_eq!(
            open(
                &cookie,
                SecretReveal::FeedToken,
                "v1.garbage.value",
                7,
                1_000
            ),
            None
        );
    }
}
