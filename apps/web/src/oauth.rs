//! The Google and GitHub authorization protocols: building the authorization URL, exchanging the authorization code for
//! tokens, and fetching the identity the provider confirmed. Binding, sign-in and registration are in
//! [`crate::identity::external`]; the credentials (Client ID and encrypted Client Secret) are stored with the SMS
//! provider's in the `login.providers` row ([`crate::sms`]).
//!
//! - Both use `state` and PKCE (S256): `state` and the verifier go into the encrypted cookie written when starting
//!   authorization and are checked on the callback.
//! - Google is OpenID Connect: the ID Token is fetched by this site's server directly from the token endpoint over
//!   TLS, so per the spec the signature need not be verified; only `iss`, `aud`, `exp` and `nonce` are checked. The
//!   identity is `sub`, and the e-mail counts as verified when `email_verified` is true.
//! - GitHub is OAuth 2.0 (user authorization of a GitHub App): the identity is the numeric `id`; the e-mail is the
//!   entry in `/user/emails` that is "primary and verified", which cannot be read when the App lacks the read-only
//!   "Email addresses" permission and is then treated as no e-mail. API calls must send a `User-Agent`.
//! - Production URLs are constants; tests replace them with local fake services ([`Endpoints`] hangs on the
//!   deployment config but is not read from the config file and can only be changed in code).
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail, ensure};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use bytes::Bytes;
use http_body_util::Full;
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::outbound::{Method, Request, header};

const TIMEOUT: Duration = Duration::from_secs(10);
const RESPONSE_LIMIT: usize = 256 * 1024;
const USER_AGENT: &str = "gono-forum-web";
const GOOGLE_ISSUERS: &[&str] = &["https://accounts.google.com", "accounts.google.com"];
/// Allowed clock skew for the ID Token expiry.
const CLOCK_SKEW: i64 = 300;

/// Third-party sign-in providers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Google,
    Github,
}

impl Provider {
    pub const ALL: [Self; 2] = [Self::Google, Self::Github];

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "google" => Some(Self::Google),
            "github" => Some(Self::Github),
            _ => None,
        }
    }

    /// The spelling stored in the binding table, settings and URLs.
    pub fn value(self) -> &'static str {
        match self {
            Self::Google => "google",
            Self::Github => "github",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Google => "Google",
            Self::Github => "GitHub",
        }
    }
}

/// Application credentials saved in the admin area (without the secret).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OAuthApp {
    pub client_id: String,
    #[serde(skip)]
    pub has_secret: bool,
}

impl OAuthApp {
    /// Usable only when both the Client ID and the Client Secret are present.
    pub fn configured(&self) -> bool {
        !self.client_id.is_empty() && self.has_secret
    }
}

/// The provider's URLs.
#[derive(Clone, Debug)]
pub struct Endpoints {
    pub google_authorize: String,
    pub google_token: String,
    pub github_authorize: String,
    pub github_token: String,
    /// Root URL of the GitHub API (`/user` and `/user/emails` are appended).
    pub github_api: String,
    /// Local fake services in tests use plain HTTP (only 127.0.0.1 is accepted, see
    /// [`crate::outbound::send_loopback`]).
    pub loopback: bool,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            google_authorize: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            google_token: "https://oauth2.googleapis.com/token".into(),
            github_authorize: "https://github.com/login/oauth/authorize".into(),
            github_token: "https://github.com/login/oauth/access_token".into(),
            github_api: "https://api.github.com".into(),
            loopback: false,
        }
    }
}

/// One-time values generated when starting authorization: `state` prevents forged cross-site callbacks, `nonce` binds
/// Google's ID Token, and `verifier` is the PKCE verifier.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Challenge {
    pub state: String,
    pub nonce: String,
    pub verifier: String,
}

fn random_url_safe(bytes: usize) -> String {
    let mut buffer = vec![0_u8; bytes];
    OsRng.fill_bytes(&mut buffer);
    URL_SAFE_NO_PAD.encode(buffer)
}

impl Challenge {
    pub fn new() -> Self {
        Self {
            state: random_url_safe(24),
            nonce: random_url_safe(24),
            // 32 bytes encode to 43 characters, within the 43–128 PKCE requires.
            verifier: random_url_safe(32),
        }
    }

    /// PKCE's `code_challenge`: the verifier's SHA-256 in base64url (without padding).
    pub fn code_challenge(&self) -> String {
        URL_SAFE_NO_PAD.encode(Sha256::digest(self.verifier.as_bytes()))
    }
}

impl Default for Challenge {
    fn default() -> Self {
        Self::new()
    }
}

/// The authorization URL: the browser goes from here to the provider's sign-in and consent page.
pub fn authorize_url(
    endpoints: &Endpoints,
    provider: Provider,
    client_id: &str,
    redirect_uri: &str,
    challenge: &Challenge,
) -> String {
    let code_challenge = challenge.code_challenge();
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("state", &challenge.state)
        .append_pair("code_challenge", &code_challenge)
        .append_pair("code_challenge_method", "S256");
    let base = match provider {
        Provider::Google => {
            query
                .append_pair("response_type", "code")
                .append_pair("scope", "openid email profile")
                .append_pair("nonce", &challenge.nonce)
                .append_pair("prompt", "select_account");
            &endpoints.google_authorize
        }
        Provider::Github => &endpoints.github_authorize,
    };
    format!("{base}?{}", query.finish())
}

/// The identity the provider confirmed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    /// Google's `sub`, GitHub's numeric `id`.
    pub subject: String,
    /// For display on the account security page and in the admin area: the Google e-mail (the name when there is
    /// none), GitHub's `login`.
    pub display: String,
    /// The e-mail the provider verified; `None` when absent or unverified.
    pub email: Option<String>,
    /// The username suggestion prefilled on "Finish signing up".
    pub suggestion: String,
}

async fn send(
    endpoints: &Endpoints,
    request: Request<Full<Bytes>>,
) -> Result<crate::outbound::Response> {
    if endpoints.loopback {
        crate::outbound::send_loopback(request, TIMEOUT, RESPONSE_LIMIT).await
    } else {
        crate::outbound::send(request, TIMEOUT, RESPONSE_LIMIT).await
    }
}

fn form_body(pairs: &[(&str, &str)]) -> String {
    let mut body = url::form_urlencoded::Serializer::new(String::new());
    for (name, value) in pairs {
        body.append_pair(name, value);
    }
    body.finish()
}

/// Exchange the authorization code from the callback for an identity. `redirect_uri` must match the one used when
/// starting authorization character for character.
pub async fn exchange(
    endpoints: &Endpoints,
    provider: Provider,
    client_id: &str,
    secret: &str,
    redirect_uri: &str,
    code: &str,
    challenge: &Challenge,
) -> Result<Profile> {
    ensure!(
        !code.is_empty() && code.len() <= 2048,
        "the authorization code is empty or too long"
    );
    match provider {
        Provider::Google => {
            google(endpoints, client_id, secret, redirect_uri, code, challenge).await
        }
        Provider::Github => {
            github(endpoints, client_id, secret, redirect_uri, code, challenge).await
        }
    }
}

async fn google(
    endpoints: &Endpoints,
    client_id: &str,
    secret: &str,
    redirect_uri: &str,
    code: &str,
    challenge: &Challenge,
) -> Result<Profile> {
    let body = form_body(&[
        ("code", code),
        ("client_id", client_id),
        ("client_secret", secret),
        ("redirect_uri", redirect_uri),
        ("grant_type", "authorization_code"),
        ("code_verifier", &challenge.verifier),
    ]);
    let request = Request::builder()
        .method(Method::POST)
        .uri(&endpoints.google_token)
        .header(header::ACCEPT, "application/json")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::USER_AGENT, USER_AGENT)
        .body(Full::new(Bytes::from(body)))
        .context("failed to build the Google token request")?;
    let response = send(endpoints, request).await?;
    #[derive(Deserialize)]
    struct Reply {
        #[serde(default)]
        id_token: String,
        #[serde(default)]
        error: String,
    }
    let reply: Reply = serde_json::from_slice(&response.body).map_err(|_| {
        anyhow!(
            "the Google token endpoint returned an unrecognized response (HTTP {})",
            response.status
        )
    })?;
    if response.status != 200 || reply.id_token.is_empty() {
        bail!(
            "the Google token endpoint rejected the authorization code (HTTP {}): {}",
            response.status,
            reply.error
        );
    }
    google_profile(
        &reply.id_token,
        client_id,
        &challenge.nonce,
        crate::identity::now(),
    )
}

/// Check Google's ID Token (without verifying the signature, see the module docs) and extract the identity.
fn google_profile(id_token: &str, client_id: &str, nonce: &str, now: i64) -> Result<Profile> {
    let mut parts = id_token.split('.');
    let (Some(_header), Some(payload), Some(_signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        bail!("the ID token does not have three parts");
    };
    let payload = URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .context("the ID token payload is not base64url")?;
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Audience {
        One(String),
        Many(Vec<String>),
    }
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Flag {
        Bool(bool),
        Text(String),
    }
    #[derive(Deserialize)]
    struct Claims {
        iss: String,
        aud: Audience,
        exp: i64,
        #[serde(default)]
        nonce: String,
        sub: String,
        #[serde(default)]
        email: Option<String>,
        #[serde(default)]
        email_verified: Option<Flag>,
        #[serde(default)]
        name: Option<String>,
    }
    let claims: Claims = serde_json::from_slice(&payload).context("malformed ID token payload")?;
    ensure!(
        GOOGLE_ISSUERS.contains(&claims.iss.as_str()),
        "the ID token was not issued by Google"
    );
    let audience_ok = match &claims.aud {
        Audience::One(aud) => aud == client_id,
        Audience::Many(list) => list.iter().any(|aud| aud == client_id),
    };
    ensure!(audience_ok, "the ID token was not issued for this site");
    ensure!(claims.exp + CLOCK_SKEW > now, "the ID token has expired");
    ensure!(
        !nonce.is_empty() && claims.nonce == nonce,
        "the ID token nonce does not match the one sent with the authorization"
    );
    ensure!(
        (1..=255).contains(&claims.sub.len()) && claims.sub.bytes().all(|b| b.is_ascii_graphic()),
        "malformed ID token sub"
    );
    let verified = matches!(claims.email_verified, Some(Flag::Bool(true)))
        || matches!(&claims.email_verified, Some(Flag::Text(text)) if text == "true");
    let email = claims
        .email
        .filter(|email| verified && !email.trim().is_empty());
    let name = claims
        .name
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty());
    let suggestion = name
        .clone()
        .or_else(|| {
            email
                .as_deref()
                .and_then(|email| email.split('@').next())
                .map(str::to_owned)
        })
        .unwrap_or_default();
    let display = email.clone().or(name).unwrap_or_else(|| claims.sub.clone());
    Ok(Profile {
        subject: claims.sub,
        display: clip(&display),
        email,
        suggestion: suggest(&suggestion),
    })
}

async fn github(
    endpoints: &Endpoints,
    client_id: &str,
    secret: &str,
    redirect_uri: &str,
    code: &str,
    challenge: &Challenge,
) -> Result<Profile> {
    let body = form_body(&[
        ("client_id", client_id),
        ("client_secret", secret),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", &challenge.verifier),
    ]);
    let request = Request::builder()
        .method(Method::POST)
        .uri(&endpoints.github_token)
        .header(header::ACCEPT, "application/json")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::USER_AGENT, USER_AGENT)
        .body(Full::new(Bytes::from(body)))
        .context("failed to build the GitHub token request")?;
    let response = send(endpoints, request).await?;
    #[derive(Deserialize)]
    struct Token {
        #[serde(default)]
        access_token: String,
        #[serde(default)]
        error: String,
    }
    let token: Token = serde_json::from_slice(&response.body).map_err(|_| {
        anyhow!(
            "the GitHub token endpoint returned an unrecognized response (HTTP {})",
            response.status
        )
    })?;
    if response.status != 200 || token.access_token.is_empty() {
        bail!(
            "the GitHub token endpoint rejected the authorization code (HTTP {}): {}",
            response.status,
            token.error
        );
    }
    let api = endpoints.github_api.trim_end_matches('/');
    let user = github_get(endpoints, &format!("{api}/user"), &token.access_token).await?;
    if user.status != 200 {
        bail!("failed to read the GitHub user (HTTP {})", user.status);
    }
    #[derive(Deserialize)]
    struct User {
        id: u64,
        #[serde(default)]
        login: String,
    }
    let user: User = serde_json::from_slice(&user.body).context("malformed GitHub user")?;
    // Without the e-mail permission it is 403/404: treated as no e-mail, not a failure.
    let emails = github_get(
        endpoints,
        &format!("{api}/user/emails"),
        &token.access_token,
    )
    .await?;
    let email = if emails.status == 200 {
        #[derive(Deserialize)]
        struct Email {
            email: String,
            #[serde(default)]
            primary: bool,
            #[serde(default)]
            verified: bool,
        }
        serde_json::from_slice::<Vec<Email>>(&emails.body)
            .unwrap_or_default()
            .into_iter()
            .find(|email| email.primary && email.verified)
            .map(|email| email.email)
    } else {
        None
    };
    let subject = user.id.to_string();
    let display = if user.login.is_empty() {
        subject.clone()
    } else {
        user.login.clone()
    };
    Ok(Profile {
        subject,
        display: clip(&display),
        email,
        suggestion: suggest(&user.login),
    })
}

async fn github_get(
    endpoints: &Endpoints,
    uri: &str,
    token: &str,
) -> Result<crate::outbound::Response> {
    let request = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header(header::ACCEPT, "application/vnd.github+json")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::USER_AGENT, USER_AGENT)
        .header("x-github-api-version", "2022-11-28")
        .body(Full::new(Bytes::new()))
        .context("failed to build the GitHub API request")?;
    send(endpoints, request).await
}

/// The username prefilled on "Finish signing up": strips whitespace, control characters, `@` and path separators that
/// usernames may not contain, and truncates to 50 characters. Only a suggestion; on submit it is validated against the
/// username rules as usual.
fn suggest(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !(ch.is_whitespace() || ch.is_control() || matches!(ch, '@' | '/' | '\\')))
        .take(50)
        .collect()
}

/// Display fields are truncated to 255 characters (the column width of the binding table's `display`), with control
/// characters removed.
fn clip(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !ch.is_control())
        .take(255)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(claims: serde_json::Value) -> String {
        format!(
            "{}.{}.sig",
            URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256"}"#),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        )
    }

    #[test]
    fn pkce_challenge_follows_rfc_7636() {
        // The example from RFC 7636 appendix B.
        let challenge = Challenge {
            state: String::new(),
            nonce: String::new(),
            verifier: "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into(),
        };
        assert_eq!(
            challenge.code_challenge(),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let fresh = Challenge::new();
        assert_eq!(fresh.verifier.len(), 43);
        assert_ne!(fresh.state, Challenge::new().state);
    }

    #[test]
    fn authorize_urls_carry_state_and_pkce() {
        let challenge = Challenge::new();
        let endpoints = Endpoints::default();
        let google = authorize_url(
            &endpoints,
            Provider::Google,
            "id.apps.googleusercontent.com",
            "https://test.example.com/auth/google/callback",
            &challenge,
        );
        let parsed = url::Url::parse(&google).unwrap();
        let query: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(parsed.host_str(), Some("accounts.google.com"));
        assert_eq!(
            query["redirect_uri"],
            "https://test.example.com/auth/google/callback"
        );
        assert_eq!(query["state"], challenge.state);
        assert_eq!(query["nonce"], challenge.nonce);
        assert_eq!(query["code_challenge_method"], "S256");
        assert_eq!(query["scope"], "openid email profile");
        let github = authorize_url(
            &endpoints,
            Provider::Github,
            "Iv23example",
            "https://privacy.example.com/feedback/auth/github/callback",
            &challenge,
        );
        let parsed = url::Url::parse(&github).unwrap();
        let query: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(parsed.host_str(), Some("github.com"));
        assert_eq!(query["code_challenge"], challenge.code_challenge());
        assert!(!query.contains_key("nonce"));
    }

    #[test]
    fn google_id_tokens_are_checked() {
        let now = 1_800_000_000;
        let base = serde_json::json!({
            "iss": "https://accounts.google.com", "aud": "client", "exp": now + 3600,
            "nonce": "n-1", "sub": "1234567890", "email": "Person@Example.com",
            "email_verified": true, "name": "Coin Farmer"
        });
        let profile = google_profile(&token(base.clone()), "client", "n-1", now).unwrap();
        assert_eq!(profile.subject, "1234567890");
        assert_eq!(profile.email.as_deref(), Some("Person@Example.com"));
        assert_eq!(profile.display, "Person@Example.com");
        assert_eq!(
            profile.suggestion, "CoinFarmer",
            "the suggested username drops spaces"
        );
        let with = |key: &str, value: serde_json::Value| {
            let mut claims = base.clone();
            claims[key] = value;
            google_profile(&token(claims), "client", "n-1", now)
        };
        assert!(with("iss", "https://evil.example".into()).is_err());
        assert!(with("aud", "other".into()).is_err());
        assert!(with("aud", serde_json::json!(["other", "client"])).is_ok());
        assert!(with("exp", (now - CLOCK_SKEW - 1).into()).is_err());
        assert!(with("nonce", "n-2".into()).is_err());
        assert!(with("sub", "".into()).is_err());
        let unverified = with("email_verified", false.into()).unwrap();
        assert_eq!(unverified.email, None, "an unverified e-mail is not used");
        assert_eq!(unverified.display, "Coin Farmer");
        assert!(
            with("email_verified", "true".into())
                .unwrap()
                .email
                .is_some()
        );
        assert!(google_profile("a.b", "client", "n-1", now).is_err());
        assert!(
            google_profile(&token(base), "client", "", now).is_err(),
            "no nonce does not pass"
        );
    }

    #[test]
    fn providers_parse_and_label() {
        for provider in Provider::ALL {
            assert_eq!(Provider::parse(provider.value()), Some(provider));
        }
        assert_eq!(Provider::parse("twitter"), None);
        assert_eq!(Provider::Github.label(), "GitHub");
        assert!(!OAuthApp::default().configured());
        assert_eq!(suggest(" a/b\\c@d\te "), "abcde");
        // Cut by characters, not bytes: 60 three-byte characters ("\u{8393}", the Chinese for "berry") keep 50.
        assert_eq!(suggest(&"\u{8393}".repeat(60)).chars().count(), 50);
    }
}
