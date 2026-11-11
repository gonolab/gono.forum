//! Account security notice mail: one is sent on enabling and disabling two-factor authentication, regenerating
//! recovery codes, signing in with a recovery code, being locked after repeated wrong codes, a command-line reset,
//! binding, rebinding and unbinding a mobile number, binding and unbinding Google and GitHub, and setting a
//! password. Cannot be turned off; only sent to a verified, unchanged e-mail address.
//!
//! The mail gives the operation time, IP, a "browser · system" summary and the `/my/account` link; it contains no
//! authenticator secret, verification code, recovery code, password or token of any kind. The IP and UA are read
//! from that log entry at delivery time; the Outbox payload does not carry them.
use super::*;
use crate::identity::external::{LINK_LOG, UNLINK_LOG};
use crate::identity::phone::{
    MOBILE_BIND_LOG, MOBILE_CHANGE_LOG, MOBILE_UNBIND_LOG, PASSWORD_SET_LOG,
};
use crate::identity::two_factor::{
    DISABLE_LOG, ENABLE_LOG, LOCKOUT_LOG, REGENERATE_LOG, RESET_LOG, SecurityNoticeMail,
};

pub(super) use crate::identity::two_factor::NOTICE_EVENT as EVENT_TYPE;

/// Which event this log entry stands for; nothing is sent for other kinds.
fn event_text(lang: crate::i18n::Lang, log_type: &str, data: &serde_json::Value) -> Option<String> {
    Some(match log_type {
        ENABLE_LOG => lang.mail_security_enable().into(),
        DISABLE_LOG => lang.mail_security_disable().into(),
        REGENERATE_LOG => lang.mail_security_regenerate().into(),
        LOCKOUT_LOG => lang.mail_security_lockout().into(),
        RESET_LOG => lang.mail_security_reset().into(),
        MOBILE_BIND_LOG => lang.mail_security_mobile_bind(masked(data, "mobile")),
        MOBILE_CHANGE_LOG => {
            lang.mail_security_mobile_change(masked(data, "from"), masked(data, "mobile"))
        }
        MOBILE_UNBIND_LOG => lang.mail_security_mobile_unbind(masked(data, "mobile")),
        PASSWORD_SET_LOG => lang.mail_security_password_set().into(),
        LINK_LOG => lang.mail_security_link(provider(lang, data), masked(data, "display")),
        UNLINK_LOG => lang.mail_security_unlink(provider(lang, data), masked(data, "display")),
        "identity.login" if data.get("method").and_then(|v| v.as_str()) == Some("recovery") => {
            let remaining = data
                .get("recovery_remaining")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0);
            lang.mail_security_recovery_login(remaining)
        }
        _ => return None,
    })
}

/// The provider in the log, written as its name (Google, GitHub).
fn provider(lang: crate::i18n::Lang, data: &serde_json::Value) -> &'static str {
    data.get("provider")
        .and_then(|v| v.as_str())
        .and_then(crate::oauth::Provider::parse)
        .map_or(
            lang.mail_security_other_provider(),
            crate::oauth::Provider::label,
        )
}

/// The log records the masked number; "***" when it cannot be read.
fn masked<'a>(data: &'a serde_json::Value, key: &str) -> &'a str {
    data.get(key).and_then(|v| v.as_str()).unwrap_or("***")
}

pub(super) async fn deliver(
    mailer: &Mailer,
    settings: &settings::Stored,
    mail: &ClaimedMail,
    clock: &(impl Fn() -> i64 + Sync),
) -> Result<Delivery> {
    let Ok(payload) = serde_json::from_str::<SecurityNoticeMail>(&mail.payload) else {
        return Ok(Delivery::Skipped("security notice payload is malformed"));
    };
    if payload.uid != mail.aggregate_id
        || payload.uid <= 0
        || payload.log_id <= 0
        || payload.email_hash.len() != 64
    {
        return Ok(Delivery::Skipped("security notice payload is malformed"));
    }
    let db = &mailer.database;
    let mut tx = db.begin_write().await?;
    let member = crate::db::query("SELECT username,email,emailstatus,`freeze`,account_state FROM gonof_common_member WHERE uid=? FOR UPDATE")
        .bind(payload.uid)
        .fetch_optional(db.observe(&mut *tx))
        .await?;
    let Some(member) = member else {
        tx.commit().await?;
        return Ok(Delivery::Skipped("the recipient account does not exist"));
    };
    if member.try_get::<i64, _>("freeze")? != 0 || db::text(&member, "account_state")? != "active" {
        tx.commit().await?;
        return Ok(Delivery::Skipped(
            "the recipient account is frozen, deleted or inactive",
        ));
    }
    let to: Option<String> = member.try_get("email")?;
    let Some(to) = to.filter(|value| !value.is_empty()) else {
        tx.commit().await?;
        return Ok(Delivery::Skipped(
            "the recipient account has no email address",
        ));
    };
    if member.try_get::<i64, _>("emailstatus")? != 1
        || crate::identity::token_hash(&to) != payload.email_hash
    {
        tx.commit().await?;
        return Ok(Delivery::Skipped(
            "the recipient email address is unverified or changed",
        ));
    }
    let log = crate::db::query("SELECT uid,type,data,dateline FROM gonof_common_log WHERE id=?")
        .bind(payload.log_id)
        .fetch_optional(db.observe(&mut *tx))
        .await?;
    let Some(log) =
        log.filter(|row| row.try_get::<Option<i64>, _>("uid").ok().flatten() == Some(payload.uid))
    else {
        tx.commit().await?;
        return Ok(Delivery::Skipped("the log record no longer exists"));
    };
    let data: serde_json::Value =
        serde_json::from_str(&db::text(&log, "data")?).unwrap_or_default();
    let log_type = db::text(&log, "type")?;
    // Inside the transaction only whether the entry is a security event matters; the text is worded after commit.
    if event_text(crate::i18n::Lang::EN, &log_type, &data).is_none() {
        tx.commit().await?;
        return Ok(Delivery::Skipped("the log record is not a security notice"));
    };
    let field = |name: &str| {
        data.get(name)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let ip = Some(field("client_ip"))
        .filter(|ip| !ip.is_empty())
        .unwrap_or_default();
    let user_agent = field("user_agent");
    let device = if user_agent.is_empty() {
        String::new()
    } else {
        user_agent.clone()
    };
    let dateline: i64 = log.try_get("dateline")?;
    hold_lease(
        db,
        &mut tx,
        mail,
        clock(),
        "the security notice delivery lease expired",
    )
    .await?;
    // Commit first after the recheck and lease renewal, then render, write the draft and send outside the lock (on
    // SQLite the whole site has a single writer, so a write transaction neither reads templates nor writes to disk).
    // The lease is fully renewed, so no other deliverer can claim this mail; the draft is written to disk before
    // sending, and a failed write after commit is retried as usual.
    tx.commit().await?;
    let site = mailer.site_settings().await?;
    let lang = site.site.lang();
    let site_name = mailer.site_name(None).await?;
    let event = event_text(lang, &log_type, &data).unwrap_or_default();
    let ip = if ip.is_empty() {
        lang.mail_unknown().to_owned()
    } else {
        ip
    };
    let device = if device.is_empty() {
        lang.mail_unknown_device().to_owned()
    } else {
        crate::identity::device_summary(lang, &device)
    };
    let time = templates::mail_time(dateline, &crate::clock::TimeFormat::from(&*site));
    let username = db::text(&member, "username")?;
    let link = crate::mail::site_link(&mailer.site_url().await?, "/my/account")?;
    let values = BTreeMap::from([
        ("username".into(), username.clone()),
        ("event".into(), event.clone()),
        ("time".into(), time.clone()),
        ("ip".into(), ip.clone()),
        ("device".into(), device.clone()),
        ("link".into(), link.to_string()),
        ("site_name".into(), site_name.clone()),
    ]);
    let default_body = format!(
        "{}\r\n\r\n{}\r\n{}\r\n{}\r\n{}\r\n\r\n{}\r\n{link}\r\n\r\n{}\r\n{site_name}\r\n",
        lang.mail_greeting(username.as_str()),
        lang.mail_security_event(event.as_str()),
        lang.mail_field_time(time.as_str()),
        lang.mail_field_ip(ip.as_str()),
        lang.mail_field_device(device.as_str()),
        lang.mail_security_advice(),
        lang.mail_security_required()
    );
    let (subject, body) = templates::render_in(
        db,
        "security_notice",
        lang,
        &values,
        lang.mail_security_subject(),
        &default_body,
    )
    .await?;
    let draft = mailer
        .prepared(mail, lang, &site_name, subject, body)
        .await?;
    mailer.publish_prepared(settings, mail, &to, &draft).await?;
    confirm_sent(db, mail).await?;
    Ok(Delivery::Sent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_security_events_have_text_and_none_carries_secrets() {
        let recovery = serde_json::json!({"method": "recovery", "recovery_remaining": 3});
        assert!(
            event_text(crate::i18n::Lang::EN, "identity.login", &recovery)
                .unwrap()
                .contains("3 codes are left")
        );
        assert!(
            event_text(
                crate::i18n::Lang::EN,
                "identity.login",
                &serde_json::json!({"method": "totp"})
            )
            .is_none()
        );
        assert!(
            event_text(
                crate::i18n::Lang::EN,
                "identity.login",
                &serde_json::json!({})
            )
            .is_none()
        );
        assert!(
            event_text(
                crate::i18n::Lang::EN,
                "identity.password.change",
                &serde_json::json!({})
            )
            .is_none()
        );
        for log_type in [
            ENABLE_LOG,
            DISABLE_LOG,
            REGENERATE_LOG,
            LOCKOUT_LOG,
            RESET_LOG,
        ] {
            assert!(
                event_text(crate::i18n::Lang::EN, log_type, &serde_json::json!({})).is_some(),
                "{log_type}"
            );
        }
    }
}
