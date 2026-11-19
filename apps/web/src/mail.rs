//! Account mail uses the existing transactional outbox and an administrator-selected transport.
mod branding;
mod content;
mod governance;
mod login_alert;
mod message;
mod security_notice;
mod settings;
pub mod templates;
mod transport;
use crate::db::Tx;
pub use settings::{MailInput, MailSettings};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    db::{self, Database},
    identity::{random_token, token_hash},
};

const LEASE_SECONDS: i64 = 60;
const BATCH_SIZE: usize = 50;

/// Every outbox event type the mail worker delivers: account and moderation mail first, then content notification
/// mail and private message mail alerts ([`MAIL_CLAIM_ORDER`] claims them in these two tiers). The admin task list
/// labels these as mail, and retention cleanup deletes them once sent (`maintenance.rs`).
pub const EVENT_TYPES: [&str; 12] = [
    "email_verification",
    "password_reset",
    "email_change",
    "appeal_access",
    "privacy_access",
    "sanction_issued",
    "sanction_ended",
    "appeal_result",
    login_alert::EVENT_TYPE,
    security_notice::EVENT_TYPE,
    "notification.email",
    crate::messages::mail::EVENT_TYPE,
];

/// Whether the mail library accepts `value` as an address it can send to. Every address a member enters goes
/// through this as well (`identity::normalize_email`), so an account never holds an address its mail cannot reach.
pub(crate) fn valid_address(value: &str) -> bool {
    value.parse::<lettre::Address>().is_ok()
}

#[derive(Clone)]
pub struct Mailer {
    database: Database,
    /// Private config `[common]`: decrypts the SMTP authorization code saved in the admin area.
    config: crate::secrets::CommonConfig,
    directory: PathBuf,
    /// The process-wide global cache: the forum URL and time style for links are read from here
    /// (docs/settings-and-caching.md, "Reading settings"). The same handle as the one on `AppState`.
    global: crate::cache::GlobalCache,
}

/// The finished mail of one event, kept in `mail/prepared/{eventid}.eml` until the event is finished so that a retry
/// sends the same bytes.
///
/// Fields this version does not know are ignored rather than refused, so a version rolled back to still reads the
/// drafts a later one wrote. Versions up to 0.2.0 refused them; docs/install.md, "Upgrades", says to empty the folder
/// when rolling back to one of those.
#[derive(Serialize, Deserialize)]
struct PreparedMail {
    /// Put before the subject when the mail goes out over SMTP: the site name in the mail's language, such as
    /// "[Gono Forum] " (zh-CN wraps the name in full-width brackets instead). Drafts written before it existed have
    /// none.
    #[serde(default)]
    subject_prefix: String,
    subject: String,
    body: String,
    content_kind: Option<crate::notifications::preferences::Kind>,
    html: Option<String>,
    /// The event the draft was written for: its key, recipient account and queue time. Event ids are not unique
    /// across a database restore: a backup restored under a newer `mail/prepared/` gives the next events the ids of
    /// drafts written for other members. A draft that does not match all three, or was written before they were
    /// recorded, is discarded and the mail written again ([`Mailer::read_prepared`]).
    #[serde(default)]
    event_key: String,
    #[serde(default)]
    aggregate_id: i64,
    #[serde(default)]
    dateline: i64,
}

impl PreparedMail {
    fn written_for(&self, mail: &ClaimedMail) -> bool {
        !self.event_key.is_empty()
            && self.event_key == mail.event_key
            && self.aggregate_id == mail.aggregate_id
            && self.dateline == mail.dateline
    }
}

impl Mailer {
    pub fn new(
        database: Database,
        config: crate::secrets::CommonConfig,
        directory: PathBuf,
        global: crate::cache::GlobalCache,
    ) -> Self {
        Self {
            database,
            config,
            directory,
            global,
        }
    }

    /// Site settings, from the process-wide global cache.
    pub(super) async fn site_settings(
        &self,
    ) -> Result<std::sync::Arc<crate::runtime::SiteSettings>> {
        Ok(self
            .global
            .settings(&self.database)
            .await
            .map_err(|error| match error {
                crate::identity::IdentityError::Internal(inner) => inner,
                other => anyhow::anyhow!(other.to_string()),
            })?
            .settings
            .clone())
    }

    /// The site URL for links in mail bodies: the forum URL from the admin area's "Global settings › Site settings"
    /// (`settings.site.url`, with the path prefix in a sub-path deployment). Mail only uses the main site; pass it to
    /// [`site_link`] to build URLs.
    ///
    /// Mail has no request to derive a `Host` from, so an empty forum URL is an error, with no fallback to any default
    /// URL: delivery fails and retries with backoff, and once the administrator fills in the URL the next round goes
    /// out.
    /// Read on every delivery (there is a process cache), so changing the URL needs no restart.
    pub(super) async fn site_url(&self) -> Result<String> {
        let settings = self.site_settings().await?;
        if settings.site.url.is_empty() {
            return Err(anyhow::Error::new(crate::runtime::SettingsError::new(
                "settings.site.url",
                Some(""),
                crate::i18n::current().mail_error_site_url(),
            )));
        }
        Ok(settings.site.url.clone())
    }

    /// The site URL for content notification links: the canonical domain of the thread's top-level forum; the forum URL
    /// when multi-domain is off, the canonical is the primary domain, or the forum is shown nowhere.
    pub(super) async fn forum_site_url(&self, top_fid: i64) -> Result<String> {
        let url = self.site_url().await?;
        let Some(map) = self.domain_map().await? else {
            return Ok(url);
        };
        Ok(match map.home(&url, top_fid) {
            crate::domains::Home::Other(row) => row.site_url.clone(),
            _ => url,
        })
    }

    /// The site URL for account mail links: the other domain the request started on, if it is still enabled and
    /// still not the primary domain; otherwise the forum URL.
    async fn account_site_url(&self, domain_id: Option<i64>) -> Result<String> {
        let url = self.site_url().await?;
        let (Some(domain_id), Some(map)) = (domain_id, self.domain_map().await?) else {
            return Ok(url);
        };
        Ok(map
            .get(domain_id)
            .filter(|row| row.enabled && row.site_url != url)
            .map_or(url, |row| row.site_url.clone()))
    }

    /// The language of background notifications: the site language (docs/architecture.md, "Languages").
    pub(super) async fn site_language(&self) -> Result<crate::i18n::Lang> {
        Ok(self.site_settings().await?.site.lang())
    }

    /// The language of mail a member started (verification, password reset, email change): the language of the
    /// other domain the request started on, if it is still enabled and has one; otherwise the site language.
    async fn account_language(&self, domain_id: Option<i64>) -> Result<crate::i18n::Lang> {
        let settings = self.site_settings().await?;
        let site = settings.site.lang();
        let (Some(domain_id), Some(map)) = (domain_id, self.domain_map().await?) else {
            return Ok(site);
        };
        Ok(map
            .get(domain_id)
            .filter(|row| row.enabled && row.site_url != settings.site.url)
            .and_then(|row| row.language)
            .unwrap_or(site))
    }

    /// The domain table when multi-domain sites are on (process cache); `None` when off.
    async fn domain_map(&self) -> Result<Option<std::sync::Arc<crate::domains::DomainMap>>> {
        let settings = self.site_settings().await?;
        if !settings.domains.enabled {
            return Ok(None);
        }
        Ok(Some(
            self.global
                .domains(&self.database, settings.cache_ttl())
                .await?,
        ))
    }

    pub async fn deliver_pending(&self, now: i64) -> Result<DeliveryReport> {
        deliver(self, now).await
    }

    // Only call after checking current account/token/content eligibility under their locks and
    // committing that check (the lease is held, so no other worker prepares this event).
    // The recipient is deliberately excluded: it must be checked on every attempt.
    async fn prepared(
        &self,
        mail: &ClaimedMail,
        lang: crate::i18n::Lang,
        site: &str,
        subject: String,
        body: String,
    ) -> Result<PreparedMail> {
        self.prepare_with_kind(mail, lang, site, subject, body, None)
            .await
    }

    /// The site name a mail is signed with and puts before its subject (the site name, `settings.site.name` or the
    /// product name): for mail a member started on another enabled domain, that domain's own site name.
    pub(super) async fn site_name(&self, domain_id: Option<i64>) -> Result<String> {
        let settings = self.site_settings().await?;
        let main = settings.site.display_name().to_owned();
        let (Some(domain_id), Some(map)) = (domain_id, self.domain_map().await?) else {
            return Ok(main);
        };
        Ok(map
            .get(domain_id)
            .filter(|row| row.enabled && row.site_url != settings.site.url)
            .and_then(|row| row.site_name.clone())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(main))
    }

    fn prepared_path(&self, eventid: i64) -> PathBuf {
        self.directory
            .join("prepared")
            .join(format!("{eventid}.eml"))
    }

    /// Delete an event's draft once it reaches a final state (sent or abandoned).
    ///
    /// A draft exists for one purpose only: if the process crashes after sending but before confirming, the retry
    /// can send a byte-for-byte identical mail. After a final state there are no more retries, and an account
    /// mail's draft holds the full link with the token, so keeping it would leave the raw token on disk. A failed
    /// delete is only logged: the event is already finished and must not be rolled back or resent for this.
    fn discard_prepared(&self, eventid: i64) {
        match fs::remove_file(self.prepared_path(eventid)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(%error, eventid, "could not delete the draft of a finished mail");
            }
        }
    }

    /// The draft of this event, if one was written for it. A draft left under this id by another event is removed,
    /// so the mail is written again for this one and nothing of the other recipient's mail goes out. Reads the disk:
    /// call it outside any write transaction.
    fn read_prepared(&self, mail: &ClaimedMail) -> Result<Option<PreparedMail>> {
        let destination = self.prepared_path(mail.eventid);
        if !destination.exists() {
            return Ok(None);
        }
        ensure!(
            fs::symlink_metadata(&destination)?.file_type().is_file(),
            "the prepared mail is not a regular file"
        );
        let draft: PreparedMail = serde_json::from_slice(&fs::read(&destination)?)
            .context("the prepared mail is malformed")?;
        if draft.written_for(mail) {
            return Ok(Some(draft));
        }
        tracing::warn!(
            eventid = mail.eventid,
            event_type = %mail.event_type,
            "the saved draft of this mail was written for another event; writing it again"
        );
        fs::remove_file(&destination).context("could not remove a draft of another event")?;
        Ok(None)
    }

    async fn prepare_with_kind(
        &self,
        mail: &ClaimedMail,
        lang: crate::i18n::Lang,
        site: &str,
        subject: String,
        body: String,
        content_kind: Option<crate::notifications::preferences::Kind>,
    ) -> Result<PreparedMail> {
        let existing = self.read_prepared(mail)?;
        self.prepare_from(existing, mail, lang, site, subject, body, content_kind)
            .await
    }

    /// The draft to send: `existing`, read earlier with [`Self::read_prepared`], or else the mail just rendered,
    /// written to disk first.
    #[allow(clippy::too_many_arguments)]
    async fn prepare_from(
        &self,
        existing: Option<PreparedMail>,
        mail: &ClaimedMail,
        lang: crate::i18n::Lang,
        site: &str,
        subject: String,
        body: String,
        content_kind: Option<crate::notifications::preferences::Kind>,
    ) -> Result<PreparedMail> {
        if let Some(value) = existing {
            ensure!(
                value.content_kind == content_kind,
                "the prepared mail has another notification kind"
            );
            return Ok(value);
        }
        let html = branding::current_url(&self.database, &self.site_url().await?)
            .await?
            .map(|url| branding::html(lang, site, &body, &url));
        let value = PreparedMail {
            subject_prefix: lang.mail_subject_prefix(site),
            subject,
            body,
            content_kind,
            html,
            event_key: mail.event_key.clone(),
            aggregate_id: mail.aggregate_id,
            dateline: mail.dateline,
        };
        persist(
            &self.directory.join("prepared"),
            mail,
            &serde_json::to_vec(&value)?,
        )?;
        Ok(value)
    }

    /// Send a finalized mail. Only called after the transaction that rechecks eligibility has committed: SMTP
    /// takes up to 15 seconds, during which no member, forum or thread row lock may be held, otherwise every
    /// write to that account or forum would have to queue.
    async fn publish_prepared(
        &self,
        settings: &settings::Stored,
        mail: &ClaimedMail,
        to: &str,
        draft: &PreparedMail,
    ) -> Result<()> {
        ensure!(
            !to.is_empty() && !to.contains(['\r', '\n']) && !draft.subject.contains(['\r', '\n']),
            "mail header is not valid"
        );
        if settings.settings.transport == "smtp" {
            // An invalid recipient address will never go out however many times it is retried; an invalid sender
            // address is a configuration problem and is retried as usual.
            settings::address(to).map_err(|_| {
                anyhow::Error::new(Rejected(
                    "the recipient address is not valid; not retrying".into(),
                ))
            })?;
            let message = transport::prepared_message(
                &settings.settings,
                to,
                draft,
                mail.eventid,
                mail.dateline,
            )?;
            self.send_smtp(settings, message).await?;
        } else {
            ensure!(
                settings.settings.transport == "local",
                "mail transport is not valid"
            );
            let rendered = transport::local_message(to, draft, mail.eventid);
            persist(&self.directory, mail, rendered.as_bytes())?;
        }
        Ok(())
    }
}

/// The outcome of one delivery: sent, or abandoned because the eligibility no longer holds. The reason for
/// abandoning is written to `last_error`, so the admin area and logs can tell a turned-off preference, a block, an
/// invalidated token or an account change apart.
pub(super) enum Delivery {
    Sent,
    Skipped(&'static str),
}

/// A delivery failure that retries cannot fix: the recipient address is invalid, or the receiving server clearly
/// says this mailbox does not exist or is disabled. The event is terminated at once, without backoff.
///
/// 5xx errors such as failed authentication, a rejected sender, or content or rate policies are mostly site
/// configuration or provider limits; once fixed, queued mail can still go out, so they are not counted here and
/// are still retried as temporary failures.
#[derive(Debug)]
pub(super) struct Rejected(pub(super) String);

impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Rejected {}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct DeliveryReport {
    pub claimed: usize,
    pub delivered: usize,
    pub failed: usize,
    pub skipped: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityMail {
    to: String,
    username: String,
    token: String,
    /// Multi-domain sites: which enabled other domain the request started on; absent for the primary domain,
    /// unregistered Hosts and when multi-domain is off. Only the id is stored; delivery takes the URL from the domain
    /// table at that time, not from the request's Host.
    #[serde(default)]
    domain_id: Option<i64>,
}

/// Queue body of account mail (registration verification, password reset, e-mail change, data request
/// verification). Carries that row's id when started on another domain.
pub(crate) fn identity_payload(to: &str, username: &str, token: &str) -> String {
    let mut payload = serde_json::json!({"to": to, "username": username, "token": token});
    if let Some(domain_id) = crate::site::other(|row| row.domain_id) {
        payload["domain_id"] = domain_id.into();
    }
    payload.to_string()
}

struct ClaimedMail {
    eventid: i64,
    event_key: String,
    event_type: String,
    aggregate_id: i64,
    payload: String,
    attempts: i64,
    dateline: i64,
    lease_token: String,
}

async fn deliver(mailer: &Mailer, now: i64) -> Result<DeliveryReport> {
    let database = &mailer.database;
    let started = std::time::Instant::now();
    let clock = || now.saturating_add(started.elapsed().as_secs() as i64);
    let mut report = DeliveryReport::default();
    // The transport settings: changes take effect on the next bounded batch without restarting the application.
    // Read once the first mail is about to be claimed, so nothing is read while sending is off.
    let mut stored = None;
    for _ in 0..BATCH_SIZE {
        // Mail sending turned off in the admin area (`settings.email.enabled`) only stops sending: the rest of the
        // queue is left unclaimed and goes out as usual once turned back on. Read before every claim, from the process
        // cache, so a save takes effect in this process before the next mail, not after a batch that a slow SMTP
        // server can stretch to minutes.
        if !mailer.site_settings().await?.email.enabled {
            break;
        }
        if stored.is_none() {
            stored = Some(mailer.stored().await?);
        }
        let settings = stored.as_ref().expect("read above");
        let Some(mail) = claim(database, clock()).await? else {
            break;
        };
        report.claimed += 1;
        let outcome: Result<Delivery> =
            async { deliver_current(mailer, settings, &mail, &clock).await }.await;
        match outcome {
            Ok(Delivery::Skipped(reason)) => {
                abandon(mailer, &mail, reason).await?;
                report.skipped += 1;
            }
            Ok(Delivery::Sent) => {
                mailer.discard_prepared(mail.eventid);
                report.delivered += 1;
            }
            Err(error) if error.downcast_ref::<Rejected>().is_some() => {
                let description = error.to_string();
                tracing::warn!(
                    eventid = mail.eventid,
                    event_type = %mail.event_type,
                    error = %description,
                    "the mail was judged undeliverable; no more retries"
                );
                abandon(mailer, &mail, &description).await?;
                report.failed += 1;
            }
            Err(error) if mail.attempts >= crate::outbox::MAX_ATTEMPTS => {
                // Retries exhausted: stop automatic retries until an administrator fixes the fault and schedules a
                // retry in the admin area. The payload and draft are both kept, otherwise there would be no token
                // left to send on retry; the token itself expires as usual, and a retry after expiry abandons with
                // "the mail token has expired" and clears the payload.
                let description = crate::outbox::exhausted_error(mail.attempts, &error.to_string());
                tracing::warn!(
                    eventid = mail.eventid,
                    event_type = %mail.event_type,
                    attempts = mail.attempts,
                    error = %description,
                    "mail retries exhausted; automatic retries stopped"
                );
                crate::db::query("UPDATE gonof_common_outbox SET status = 'failed', lease_token = NULL, lease_until = 0, last_error = ? WHERE eventid = ? AND status = 'processing' AND lease_token = ?")
                    .bind(description)
                    .bind(mail.eventid)
                    .bind(&mail.lease_token)
                    .execute(database.executor())
                    .await?;
                report.failed += 1;
            }
            Err(error) => {
                let delay = 5_i64.saturating_mul(1_i64 << mail.attempts.min(9));
                let description: String = error.to_string().chars().take(1000).collect();
                // Error texts are all fixed hints written by us, without provider text, recipients or credentials.
                tracing::warn!(
                    eventid = mail.eventid,
                    event_type = %mail.event_type,
                    attempts = mail.attempts,
                    error = %description,
                    "mail delivery failed; will retry"
                );
                crate::db::query("UPDATE gonof_common_outbox SET status = 'pending', available_at = GREATEST(dateline, ?), lease_token = NULL, lease_until = 0, last_error = ? WHERE eventid = ? AND status = 'processing' AND lease_token = ?",)
                .bind(clock().saturating_add(delay.min(3600)))
                .bind(description)
                .bind(mail.eventid)
                .bind(&mail.lease_token)
                .execute(database.executor())
                .await?;
                report.failed += 1;
            }
        }
    }
    Ok(report)
}

/// Terminate this mail, writing the reason to `last_error`.
///
/// Abandoned account mail also clears the raw token: by then the token is most likely invalid, but not
/// necessarily in every case (for example when the account was frozen), so it must not stay in the database.
async fn abandon(mailer: &Mailer, mail: &ClaimedMail, reason: &str) -> Result<()> {
    let statement = if carries_token(&mail.event_type) {
        "UPDATE gonof_common_outbox SET status = 'failed', payload = '{}', lease_token = NULL, lease_until = 0, last_error = ? WHERE eventid = ? AND status = 'processing' AND lease_token = ?"
    } else {
        "UPDATE gonof_common_outbox SET status = 'failed', lease_token = NULL, lease_until = 0, last_error = ? WHERE eventid = ? AND status = 'processing' AND lease_token = ?"
    };
    let reason: String = reason.chars().take(1000).collect();
    let ended = crate::db::query(statement)
        .bind(reason)
        .bind(mail.eventid)
        .bind(&mail.lease_token)
        .execute(mailer.database.executor())
        .await?
        .rows_affected()
        > 0;
    if ended {
        mailer.discard_prepared(mail.eventid);
    }
    Ok(())
}

/// Claim order: account and moderation mail before content notification mail.
///
/// The two kinds share one queue; claimed together in due order, a wave of follow mail would push password reset and
/// verification mail to the back. This is two claims in code rather than sorting by type in SQL: take account and
/// moderation mail first, and only when there is none, content notification mail.
///
/// Sign-in alerts and two-factor security notices are account security and go in the first tier; private message
/// mail alerts sit with content notification mail in the second tier.
const MAIL_CLAIM_ORDER: [&[&str]; 2] = {
    let (account, content) = EVENT_TYPES.split_at(10);
    [account, content]
};

async fn claim(database: &Database, now: i64) -> Result<Option<ClaimedMail>> {
    let mut transaction = database.begin_write().await?;
    let mut found = None;
    for types in MAIL_CLAIM_ORDER {
        // Within a tier, the mail due longest (`outbox::lock_next_due`): a few entries of `gonof_outbox_claim` per
        // type, not the due mail of every queue (`gonof_outbox_delivery` has no type) nor, as both 5.7.44 and 8.4.11
        // do without a hint, every historical sent row in primary key order.
        found = crate::outbox::lock_next_due(
            database,
            &mut transaction,
            types,
            "eventid,event_key,event_type,aggregate_id,payload,attempts,dateline",
            now,
        )
        .await?;
        if found.is_some() {
            break;
        }
    }
    let Some(row) = found else {
        transaction.commit().await?;
        return Ok(None);
    };
    let mail = ClaimedMail {
        eventid: row.try_get("eventid")?,
        event_key: db::text(&row, "event_key")?,
        event_type: db::text(&row, "event_type")?,
        aggregate_id: row.try_get("aggregate_id")?,
        payload: db::text(&row, "payload")?,
        attempts: row.try_get::<i64, _>("attempts")?.saturating_add(1),
        dateline: row.try_get("dateline")?,
        lease_token: random_token(),
    };
    crate::db::query("UPDATE gonof_common_outbox SET status = 'processing', attempts = attempts + 1, lease_token = ?, lease_until = ? WHERE eventid = ?",)
    .bind(&mail.lease_token)
    .bind(now.saturating_add(LEASE_SECONDS))
    .bind(mail.eventid)
    .execute(database.observe(&mut *transaction))
    .await?;
    transaction.commit().await?;
    Ok(Some(mail))
}

async fn deliver_current(
    mailer: &Mailer,
    settings: &settings::Stored,
    mail: &ClaimedMail,
    clock: &(impl Fn() -> i64 + Sync),
) -> Result<Delivery> {
    if mail.event_type == "notification.email" {
        return content::deliver(mailer, settings, mail, clock).await;
    }
    if mail.event_type == crate::messages::mail::EVENT_TYPE {
        return message::deliver(mailer, settings, mail, clock).await;
    }
    if mail.event_type == login_alert::EVENT_TYPE {
        return login_alert::deliver(mailer, settings, mail, clock).await;
    }
    if mail.event_type == security_notice::EVENT_TYPE {
        return security_notice::deliver(mailer, settings, mail, clock).await;
    }
    if matches!(
        mail.event_type.as_str(),
        "sanction_issued" | "sanction_ended" | "appeal_result"
    ) {
        return governance::deliver(mailer, settings, mail, clock).await;
    }
    let database = &mailer.database;
    let Ok(payload) = serde_json::from_str::<IdentityMail>(&mail.payload) else {
        return Ok(Delivery::Skipped("account mail payload is malformed"));
    };
    if payload.to.is_empty() || payload.token.len() != 64 {
        return Ok(Delivery::Skipped("account mail payload is malformed"));
    }
    let mut transaction = database.begin_write().await?;
    // Match verification/password-reset lock order. Eligibility is rechecked under the lock and the mail is sent
    // after commit: a freeze or token revocation committed after the recheck cannot stop this mail already in
    // flight, but the token is validated again when used, so the link is still invalid after revocation.
    let member = crate::db::query("SELECT email, emailstatus, password, `freeze`,account_state FROM gonof_common_member WHERE uid = ? FOR UPDATE")
    .bind(mail.aggregate_id)
    .fetch_optional(database.observe(&mut *transaction))
    .await?;
    let Some(member) = member else {
        transaction.commit().await?;
        return Ok(Delivery::Skipped("the recipient account does not exist"));
    };
    let current_email: Option<String> = member.try_get("email")?;
    let wrong_email = if mail.event_type == "email_change" {
        current_email.as_deref() == Some(payload.to.as_str())
    } else {
        current_email.as_deref() != Some(payload.to.as_str())
    };
    let skipped = if !matches!(mail.event_type.as_str(), "appeal_access" | "privacy_access")
        && member.try_get::<i64, _>("freeze")? != 0
    {
        Some("the recipient account is frozen")
    } else if db::text(&member, "account_state")? != "active" {
        Some("the recipient account is deleted or inactive")
    } else if wrong_email {
        Some(if mail.event_type == "email_change" {
            "the account already uses this email address"
        } else {
            "the account email address has changed"
        })
    } else if matches!(mail.event_type.as_str(), "password_reset" | "email_change")
        && member
            .try_get::<Option<String>, _>("password")?
            .as_deref()
            .is_none_or(str::is_empty)
    {
        Some("the account has no usable password")
    } else if matches!(mail.event_type.as_str(), "appeal_access" | "privacy_access")
        && !crate::privacy::email_link_allowed(&member)?
    {
        // The links frozen members use instead of signing in: the same rule as when they are issued.
        Some("the account has no password and its email address is unverified")
    } else {
        None
    };
    if let Some(reason) = skipped {
        transaction.commit().await?;
        return Ok(Delivery::Skipped(reason));
    }
    let token = crate::db::query("SELECT id, dateline, expires_at FROM gonof_common_member_token WHERE uid = ? AND token_hash = ? AND purpose = ? AND email = ? AND consumed_at IS NULL AND revoked_at IS NULL FOR UPDATE")
    .bind(mail.aggregate_id)
    .bind(token_hash(&payload.token))
    .bind(&mail.event_type)
    .bind(&payload.to)
    .fetch_optional(database.observe(&mut *transaction))
    .await?;
    let Some(token) = token else {
        transaction.commit().await?;
        return Ok(Delivery::Skipped(
            "the mail token was used, revoked or issued again",
        ));
    };
    // Recheck the lease after waiting for account/token locks; a superseded worker cannot send.
    let now = clock();
    hold_lease(
        database,
        &mut transaction,
        mail,
        now,
        "the mail delivery lease expired; it will be claimed again",
    )
    .await?;
    if token.try_get::<i64, _>("dateline")? > now || token.try_get::<i64, _>("expires_at")? <= now {
        transaction.commit().await?;
        return Ok(Delivery::Skipped("the mail token has expired"));
    }
    // Commit first after the recheck and lease renewal, then render, write the draft and send outside the lock (on
    // SQLite the whole site has a single writer, so a write transaction neither reads templates nor writes to disk).
    // The lease is fully renewed, so no other deliverer can claim this mail; the draft is written to disk before
    // sending, and a failed write after commit is retried as usual.
    transaction.commit().await?;
    let lang = mailer.account_language(payload.domain_id).await?;
    let site = mailer.site_name(payload.domain_id).await?;
    let (_, default_subject, instruction, reminder) = template(&mail.event_type, lang)?;
    let link = link(
        &mail.event_type,
        &payload.token,
        &mailer.account_site_url(payload.domain_id).await?,
    )?;
    let default_body = format!(
        "{}\r\n\r\n{instruction}\r\n{link}\r\n\r\n{reminder}\r\n{site}\r\n",
        lang.mail_greeting(payload.username.as_str())
    );
    let values = BTreeMap::from([
        ("username".into(), payload.username.clone()),
        ("link".into(), link.to_string()),
        ("site_name".into(), site.clone()),
    ]);
    let (subject, body) = templates::render_in(
        database,
        &mail.event_type,
        lang,
        &values,
        default_subject,
        &default_body,
    )
    .await?;
    let draft = mailer.prepared(mail, lang, &site, subject, body).await?;
    mailer
        .publish_prepared(settings, mail, &payload.to, &draft)
        .await?;
    confirm_sent(database, mail).await?;
    Ok(Delivery::Sent)
}

/// Recheck the lease inside the transaction that rechecks eligibility, and renew it for a full period.
///
/// After the recheck passes, commit and release the locks first, then send outside the lock; the total SMTP
/// timeout is 15 seconds, and after a full renewal the lease cannot expire during that time, so no other process
/// claims the same mail to resend it.
async fn hold_lease(
    database: &Database,
    transaction: &mut Tx<'_>,
    mail: &ClaimedMail,
    now: i64,
    expired: &'static str,
) -> Result<()> {
    let lease = crate::db::query(
        "SELECT status, lease_token, lease_until FROM gonof_common_outbox WHERE eventid=? FOR UPDATE",
    )
    .bind(mail.eventid)
    .fetch_one(database.observe(&mut **transaction))
    .await?;
    ensure!(
        db::text(&lease, "status")? == "processing"
            && db::text(&lease, "lease_token")? == mail.lease_token
            && lease.try_get::<i64, _>("lease_until")? > now,
        "{expired}"
    );
    crate::db::query("UPDATE gonof_common_outbox SET lease_until=? WHERE eventid=?")
        .bind(now.saturating_add(LEASE_SECONDS))
        .bind(mail.eventid)
        .execute(database.observe(&mut **transaction))
        .await?;
    Ok(())
}

/// After sending outside the lock, confirm with a statement scoped to the lease.
///
/// Account mail also clears the raw token: the database keeps only the digest, so anyone who can read the database
/// or a backup gets no usable token; a retry after a crash before confirmation can still get the token. If the
/// lease was taken over by another process (this process stalled beyond the lease), this mail may be sent twice;
/// the taker confirms and cleans up the draft, and here an error takes the retry path without touching its draft.
async fn confirm_sent(database: &Database, mail: &ClaimedMail) -> Result<()> {
    let statement = if carries_token(&mail.event_type) {
        "UPDATE gonof_common_outbox SET status='sent',payload='{}',lease_token=NULL,lease_until=0,last_error=NULL WHERE eventid=? AND status='processing' AND lease_token=?"
    } else {
        "UPDATE gonof_common_outbox SET status='sent',lease_token=NULL,lease_until=0,last_error=NULL WHERE eventid=? AND status='processing' AND lease_token=?"
    };
    let confirmed = crate::db::query(statement)
        .bind(mail.eventid)
        .bind(&mail.lease_token)
        .execute(database.executor())
        .await?;
    ensure!(
        confirmed.rows_affected() == 1,
        "the mail was sent, but the delivery lease was taken over"
    );
    Ok(())
}

/// Account mail whose payload carries a one-time raw token.
fn carries_token(event_type: &str) -> bool {
    matches!(
        event_type,
        "email_verification"
            | "password_reset"
            | "email_change"
            | "appeal_access"
            | "privacy_access"
    )
}

fn template(
    event_type: &str,
    lang: crate::i18n::Lang,
) -> Result<(&'static str, &'static str, &'static str, &'static str)> {
    Ok(match event_type {
        "email_verification" => (
            "/verify-email",
            lang.mail_verify_subject(),
            lang.mail_verify_instruction(),
            lang.mail_verify_reminder(),
        ),
        "password_reset" => (
            "/reset-password",
            lang.mail_reset_subject(),
            lang.mail_reset_instruction(),
            lang.mail_reset_reminder(),
        ),
        "email_change" => (
            "/confirm-email-change",
            lang.mail_email_change_subject(),
            lang.mail_email_change_instruction(),
            lang.mail_email_change_reminder(),
        ),
        "appeal_access" => (
            "/appeals/access",
            lang.mail_appeal_access_subject(),
            lang.mail_appeal_access_instruction(),
            lang.mail_appeal_access_reminder(),
        ),
        "privacy_access" => (
            "/data/access",
            lang.mail_privacy_access_subject(),
            lang.mail_privacy_access_instruction(),
            lang.mail_privacy_access_reminder(),
        ),
        _ => anyhow::bail!("unsupported mail event type"),
    })
}

/// An on-site link in mail: the base URL (the forum URL, with the path prefix in a sub-path deployment) followed by
/// a root-relative path.
///
/// The caller passes the base URL and the function does not read the forum URL: the caller decides which site a mail
/// uses. `Url::set_path` is not used: it replaces the whole path and would drop the prefix.
pub(crate) fn site_link(base: &str, path: &str) -> Result<url::Url> {
    let link = url::Url::parse(&format!("{}{path}", base.trim_end_matches('/')))
        .context("the site address for mail is malformed")?;
    ensure!(
        matches!(link.scheme(), "http" | "https")
            && link.host_str().is_some()
            && link.username().is_empty()
            && link.password().is_none(),
        "the site address for mail must be an HTTP(S) address with a host name"
    );
    Ok(link)
}

fn link(event_type: &str, token: &str, site_url: &str) -> Result<url::Url> {
    let (path, _, _, _) = template(event_type, crate::i18n::Lang::EN)?;
    let mut link = site_link(site_url, path)?;
    link.query_pairs_mut().append_pair("token", token);
    Ok(link)
}

/// Atomically publish a fully synced file without replacing an existing delivery.
/// If a worker crashes after publication, its successor verifies and acknowledges it.
fn persist(directory: &Path, mail: &ClaimedMail, contents: &[u8]) -> Result<()> {
    fs::create_dir_all(directory).context("could not create the local mail directory")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    }
    let destination = directory.join(format!("{}.eml", mail.eventid));
    let temporary = directory.join(format!(".{}.{}.tmp", mail.eventid, mail.lease_token));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .context("could not create the temporary mail file")?;
    let _cleanup = TemporaryFile(temporary.clone());
    file.write_all(contents)
        .context("could not write the local mail")?;
    file.sync_all().context("could not sync the local mail")?;
    match fs::hard_link(&temporary, &destination) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            ensure!(
                fs::symlink_metadata(&destination)?.file_type().is_file()
                    && fs::read(&destination)? == contents,
                "an existing local mail differs from the pending event"
            );
        }
        Err(error) => return Err(error).context("could not publish the local mail file"),
    }
    File::open(directory)?
        .sync_all()
        .context("could not sync the local mail directory")?;
    Ok(())
}

struct TemporaryFile(PathBuf);

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod claim_tests {
    use super::*;

    /// The second claim tier is exactly the notification mail, so a type added to [`EVENT_TYPES`] lands in a tier on
    /// purpose; every account mail type has its own text and link.
    #[test]
    fn mail_event_types_split_into_account_and_notification_tiers() {
        assert_eq!(MAIL_CLAIM_ORDER[1], ["notification.email", "message.email"]);
        assert_eq!(
            MAIL_CLAIM_ORDER
                .iter()
                .map(|tier| tier.len())
                .sum::<usize>(),
            EVENT_TYPES.len()
        );
        for kind in MAIL_CLAIM_ORDER[0] {
            let routed = matches!(
                *kind,
                "sanction_issued" | "sanction_ended" | "appeal_result"
            ) || *kind == login_alert::EVENT_TYPE
                || *kind == security_notice::EVENT_TYPE;
            assert!(
                routed || template(kind, crate::i18n::Lang::EN).is_ok(),
                "{kind}"
            );
        }
    }

    /// Claim order: with a batch of content notification mail queued first, a later password reset mail is still
    /// claimed first; content notification mail only comes after the account mail is claimed, each tier in due order
    /// and then by eventid.
    #[tokio::test]
    async fn account_mail_is_claimed_before_queued_content_mail() {
        let services = crate::test_support::TestFixture::migrated().await.unwrap();
        let db = &services.db;
        let now = crate::identity::now();
        let enqueue = |key: String, kind: &'static str| async move {
            crate::db::query("INSERT INTO gonof_common_outbox(event_key,event_type,aggregate_type,aggregate_id,payload,dateline,available_at) VALUES(?,?,'test',1,'{}',?,?)")
                .bind(key)
                .bind(kind)
                .bind(now - 10)
                .bind(now - 10)
                .execute(db.executor())
                .await
                .unwrap();
        };
        for n in 0..5 {
            enqueue(format!("content-mail:{n}"), "notification.email").await;
        }
        enqueue("reset-mail".into(), "password_reset").await;
        enqueue("verify-mail".into(), "email_verification").await;
        let mut order = Vec::new();
        while let Some(mail) = claim(db, now).await.unwrap() {
            order.push(mail.event_type);
        }
        assert_eq!(
            order,
            [
                "password_reset",
                "email_verification",
                "notification.email",
                "notification.email",
                "notification.email",
                "notification.email",
                "notification.email",
            ]
        );
        services.cleanup().await.unwrap();
    }

    /// Each tier reads only the due mail of its own types, in `gonof_outbox_claim` order: content mail is found
    /// after the account tier came up empty, without the sent mail kept for the retention period or the backlog of
    /// other queues being read.
    #[tokio::test]
    async fn mail_claims_read_the_claim_index() {
        let services = crate::test_support::TestFixture::migrated().await.unwrap();
        let db = &services.db;
        let now = crate::identity::now();
        crate::test_support::outbox_backlog(db, &EVENT_TYPES, now)
            .await
            .unwrap();
        crate::db::query("INSERT INTO gonof_common_outbox(event_key,event_type,aggregate_type,aggregate_id,payload,dateline,available_at) VALUES('content-mail','notification.email','test',1,'{}',?,?)")
            .bind(now - 10)
            .bind(now - 10)
            .execute(db.executor())
            .await
            .unwrap();
        let profile = std::sync::Arc::new(crate::debug::trace::RequestProfile::new());
        let mail = crate::debug::trace::scope(profile.clone(), claim(db, now))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(mail.event_type, "notification.email");
        let checked = crate::test_support::assert_outbox_claim_plans(db, &profile.snapshot())
            .await
            .unwrap();
        assert_eq!(checked, 2, "one statement for each tier");
        services.cleanup().await.unwrap();
    }
}
