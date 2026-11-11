use super::*;
use crate::db::Tx;
use crate::identity::{Actor, IdentityError};
use crate::secrets::CommonConfig;
use serde::Serialize;

pub(super) const SETTING: &str = "mail.transport";

/// The authorization code is encrypted with the private config `[common] encryption_key` ([`crate::secrets`]),
/// with the associated data bound to the sending server and account.
fn encrypt(config: &CommonConfig, password: &str, settings: &MailSettings) -> Result<Vec<u8>> {
    config
        .seal(password, &credential_context(settings))
        .ok_or_else(|| anyhow::anyhow!(crate::secrets::missing_key()))
}

pub(super) fn decrypt(
    config: &CommonConfig,
    encrypted: &[u8],
    settings: &MailSettings,
) -> Result<String> {
    config
        .open(encrypted, &credential_context(settings))
        .ok_or_else(|| anyhow::anyhow!(crate::i18n::current().mail_settings_error_decrypt()))
}

fn credential_context(settings: &MailSettings) -> Vec<u8> {
    // A database edit cannot redirect a saved authorization code to another SMTP service.
    serde_json::to_vec(&(
        SETTING,
        &settings.host,
        settings.port,
        &settings.security,
        &settings.username,
    ))
    .expect("serializable credential context")
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MailSettings {
    pub revision: i64,
    pub transport: String,
    pub host: String,
    pub port: u16,
    pub security: String,
    pub username: String,
    pub from_address: String,
    pub from_name: String,
    pub incoming_host: String,
    pub incoming_port: u16,
    pub test_to: String,
    #[serde(skip)]
    pub has_password: bool,
}

impl Default for MailSettings {
    fn default() -> Self {
        Self {
            revision: 0,
            transport: "local".into(),
            host: String::new(),
            port: 465,
            security: "tls".into(),
            username: String::new(),
            from_address: String::new(),
            // A neutral fallback; a site that never saved its mail settings starts from its own name
            // ([`Mailer::stored`]).
            from_name: crate::i18n::PRODUCT_NAME.into(),
            incoming_host: String::new(),
            incoming_port: 995,
            test_to: String::new(),
            has_password: false,
        }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Stored {
    pub settings: MailSettings,
    pub password_encrypted: Vec<u8>,
}

pub struct MailInput {
    pub settings: MailSettings,
    pub password: String,
}

impl MailSettings {
    fn validate(&self) -> Result<()> {
        ensure!(
            matches!(self.transport.as_str(), "local" | "smtp"),
            crate::i18n::current().mail_settings_error_transport()
        );
        ensure!(
            matches!(self.security.as_str(), "tls" | "starttls"),
            crate::i18n::current().mail_settings_error_security()
        );
        if self.transport == "smtp" || !self.host.is_empty() {
            ensure!(
                valid_host(&self.host) && self.port > 0,
                crate::i18n::current().mail_settings_error_host()
            );
            address(&self.from_address)?;
            ensure!(
                !self.username.is_empty()
                    && self.username.len() <= 320
                    && !self.username.chars().any(char::is_control),
                crate::i18n::current().mail_settings_error_username()
            );
        }
        ensure!(
            !self.from_name.is_empty()
                && self.from_name.chars().count() <= 80
                && !self.from_name.chars().any(char::is_control),
            crate::i18n::current().mail_settings_error_from_name()
        );
        ensure!(
            self.incoming_host.is_empty()
                || (valid_host(&self.incoming_host) && self.incoming_port > 0),
            crate::i18n::current().mail_settings_error_incoming()
        );
        if !self.test_to.is_empty() {
            address(&self.test_to)?;
        }
        Ok(())
    }
}

fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host.is_ascii()
        && (host.parse::<std::net::IpAddr>().is_ok()
            || host.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            }))
}

pub(super) fn address(value: &str) -> Result<lettre::Address> {
    ensure!(
        value.len() <= 320 && !value.chars().any(char::is_whitespace),
        crate::i18n::current().mail_settings_error_address()
    );
    value
        .parse()
        .map_err(|_| anyhow::anyhow!(crate::i18n::current().mail_settings_error_address()))
}

/// Shares `runtime::admin_locked` with the other admin pages: this used to have its own implementation that missed
/// `account_state`, so an administrator in account deletion or already deleted could still read and change the
/// mail configuration with an old session.
async fn admin_locked(db: &Database, tx: &mut Tx<'_>, actor: Actor) -> Result<()> {
    crate::runtime::admin_locked(db, tx, actor).await?;
    Ok(())
}

impl Mailer {
    pub(super) async fn stored(&self) -> Result<Stored> {
        let row = crate::db::query("SELECT svalue FROM gonof_common_settings WHERE skey=?")
            .bind(SETTING)
            .fetch_optional(self.database.executor())
            .await?;
        let mut stored = decode(row.as_ref())?;
        if row.is_none() {
            // Never saved: the sender name starts as the site name, like the mail signature.
            stored.settings.from_name = self.site_settings().await?.site.display_name().to_owned();
        }
        Ok(stored)
    }

    /// Read by the admin page (administrator): read-only, checks the administrator through the connection pool, and
    /// opens no write transaction.
    pub async fn settings(&self, actor: Actor) -> Result<MailSettings> {
        crate::runtime::admin_unlocked(&self.database, actor).await?;
        Ok(summary(self.stored().await?))
    }

    fn candidate(&self, input: MailInput, current: &Stored) -> Result<Stored> {
        if input.settings.revision != current.settings.revision {
            return Err(IdentityError::Conflict(
                crate::i18n::current().mail_settings_error_stale(),
            )
            .into());
        }
        let mut settings = input.settings;
        for field in [
            &mut settings.host,
            &mut settings.username,
            &mut settings.from_address,
            &mut settings.from_name,
            &mut settings.incoming_host,
            &mut settings.test_to,
        ] {
            *field = field.trim().to_owned();
        }
        settings.validate()?;
        let password_encrypted = if input.password.is_empty() {
            ensure!(
                current.password_encrypted.is_empty()
                    || (settings.host == current.settings.host
                        && settings.username == current.settings.username),
                crate::i18n::current().mail_settings_error_password_again()
            );
            if current.password_encrypted.is_empty() {
                Vec::new()
            } else {
                let password =
                    decrypt(&self.config, &current.password_encrypted, &current.settings)?;
                encrypt(&self.config, &password, &settings)?
            }
        } else {
            ensure!(
                input.password.len() <= 4096 && !input.password.chars().any(char::is_control),
                crate::i18n::current().mail_settings_error_password()
            );
            encrypt(&self.config, &input.password, &settings)?
        };
        if settings.transport == "smtp" {
            ensure!(
                !password_encrypted.is_empty(),
                crate::i18n::current().mail_settings_error_password_first()
            );
            decrypt(&self.config, &password_encrypted, &settings)?;
        }
        settings.revision += 1;
        Ok(Stored {
            settings,
            password_encrypted,
        })
    }

    pub async fn save_settings(&self, actor: Actor, input: MailInput) -> Result<MailSettings> {
        let mut tx = self.database.begin_write().await?;
        admin_locked(&self.database, &mut tx, actor).await?;
        // Serialize the first save and later revisions, including concurrent administrators.
        crate::db::query(
            "SELECT skey FROM gonof_common_settings WHERE skey='identity.bootstrap_admin' FOR UPDATE",
        )
        .fetch_one(self.database.observe(&mut *tx))
        .await?;
        let row =
            crate::db::query("SELECT svalue FROM gonof_common_settings WHERE skey=? FOR UPDATE")
                .bind(SETTING)
                .fetch_optional(self.database.observe(&mut *tx))
                .await?;
        let stored = self.candidate(input, &decode(row.as_ref())?)?;
        crate::db::query("INSERT INTO gonof_common_settings(skey,svalue) VALUES(?,?) ON DUPLICATE KEY UPDATE svalue=VALUES(svalue)")
            .bind(SETTING).bind(serde_json::to_string(&stored)?).execute(self.database.observe(&mut *tx)).await?;
        self.audit(&mut tx, actor.uid, "mail.settings.save", serde_json::json!({"revision":stored.settings.revision,"transport":stored.settings.transport})).await?;
        tx.commit().await?;
        Ok(summary(stored))
    }

    pub async fn test_settings(&self, actor: Actor, input: MailInput) -> Result<()> {
        let mut tx = self.database.begin_write().await?;
        admin_locked(&self.database, &mut tx, actor).await?;
        let row = crate::db::query("SELECT svalue FROM gonof_common_settings WHERE skey=?")
            .bind(SETTING)
            .fetch_optional(self.database.observe(&mut *tx))
            .await?;
        let candidate = self.candidate(input, &decode(row.as_ref())?)?;
        ensure!(
            candidate.settings.transport == "smtp",
            crate::i18n::current().mail_settings_error_test_smtp()
        );
        address(&candidate.settings.test_to)?;
        // A persisted audit timestamp provides a shared, restart-safe test-send rate limit.
        let recent: i64 = crate::db::query_scalar("SELECT COUNT(*) FROM gonof_common_log WHERE operationuid=? AND type='mail.test.request' AND dateline>?")
            .bind(actor.uid).bind(crate::identity::now()-60).fetch_one(self.database.observe(&mut *tx)).await?;
        ensure!(
            recent == 0,
            crate::i18n::current().mail_settings_error_test_wait()
        );
        self.audit(
            &mut tx,
            actor.uid,
            "mail.test.request",
            serde_json::json!({}),
        )
        .await?;
        tx.commit().await?;
        let lang = self.site_language().await?;
        let site = self.site_name(None).await?;
        let message = transport::message(
            &candidate.settings,
            &candidate.settings.test_to,
            &lang.mail_subject_prefix(site.as_str()),
            lang.mail_test_subject(),
            // Plain text with CRLF line ends, signed with the site name like the built-in mail.
            format!(
                "{}\r\n",
                lang.mail_test_body(site.as_str(), self.site_url().await?)
                    .replace('\n', "\r\n")
            ),
            &format!("test-{}", random_token()),
        )?;
        let result = self.send_smtp(&candidate, message).await;
        let mut tx = self.database.begin_write().await?;
        self.audit(
            &mut tx,
            actor.uid,
            "mail.test.result",
            serde_json::json!({"accepted":result.is_ok()}),
        )
        .await?;
        tx.commit().await?;
        result
    }

    async fn audit(
        &self,
        tx: &mut Tx<'_>,
        uid: i64,
        kind: &str,
        data: serde_json::Value,
    ) -> Result<()> {
        crate::db::query("INSERT INTO gonof_common_log(uid,operationuid,type,data,source,dateline) VALUES(?,?,?,?,?,?)")
            .bind(uid).bind(uid).bind(kind).bind(data.to_string()).bind("web").bind(crate::identity::now())
            .execute(self.database.observe(&mut **tx)).await?;
        Ok(())
    }
}

fn decode(row: Option<&crate::db::DbRow>) -> Result<Stored> {
    row.map(|row| {
        serde_json::from_str(&db::text(row, "svalue")?)
            .map_err(|_| anyhow::anyhow!("stored mail settings are malformed"))
    })
    .transpose()
    .map(|v| v.unwrap_or_default())
}

fn summary(mut stored: Stored) -> MailSettings {
    stored.settings.has_password = !stored.password_encrypted.is_empty();
    stored.settings
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authenticated_encryption_rejects_wrong_keys_and_tampering() {
        let config = CommonConfig {
            encryption_key: "01".repeat(32),
        };
        let settings = MailSettings::default();
        let encrypted = encrypt(&config, "authorization-secret", &settings).unwrap();
        assert_ne!(
            encrypted,
            encrypt(&config, "authorization-secret", &settings).unwrap()
        );
        assert_eq!(
            decrypt(&config, &encrypted, &settings).unwrap(),
            "authorization-secret"
        );
        let redirected = MailSettings {
            host: "other.example.test".into(),
            ..settings.clone()
        };
        assert!(decrypt(&config, &encrypted, &redirected).is_err());
        assert!(
            !serde_json::to_string(&encrypted)
                .unwrap()
                .contains("authorization-secret")
        );
        let wrong = CommonConfig {
            encryption_key: "02".repeat(32),
        };
        assert!(decrypt(&wrong, &encrypted, &settings).is_err());
        let mut broken = encrypted;
        broken[12] ^= 1;
        assert!(decrypt(&config, &broken, &settings).is_err());
        assert!(encrypt(&CommonConfig::default(), "x", &settings).is_err());
    }
    #[test]
    fn settings_reject_header_injection_and_cleartext_transport() {
        let mut settings = MailSettings {
            transport: "smtp".into(),
            host: "smtp.qq.com".into(),
            username: "member@example.test".into(),
            from_address: "member@example.test".into(),
            ..MailSettings::default()
        };
        settings.validate().unwrap();
        settings.security = "plain".into();
        assert!(settings.validate().is_err());
        settings.security = "starttls".into();
        settings.from_name = "name\r\nBcc: other@example.test".into();
        assert!(settings.validate().is_err());
        assert!(address("member@example.test\r\nBcc:other@example.test").is_err());
        assert!(!valid_host("smtp.example.test/path"));
    }
}
