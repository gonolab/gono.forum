//! Delivery of new-device sign-in alerts.
//!
//! The mail gives the sign-in time, the full IP and a "browser · system" summary; it does not include the raw UA
//! and does no geolocation; it contains no password, session cookie, CSRF, sign-in nonce or token of any kind. The
//! IP and UA are read here from that `identity.login` log entry; the Outbox payload does not carry them.
use super::*;
use crate::{identity::login_alert::LoginAlertMail, notifications::preferences};

pub(super) use crate::identity::login_alert::EVENT_TYPE;

pub(super) async fn deliver(
    mailer: &Mailer,
    settings: &settings::Stored,
    mail: &ClaimedMail,
    clock: &(impl Fn() -> i64 + Sync),
) -> Result<Delivery> {
    let Ok(payload) = serde_json::from_str::<LoginAlertMail>(&mail.payload) else {
        return Ok(Delivery::Skipped("sign-in alert payload is malformed"));
    };
    if payload.uid != mail.aggregate_id
        || payload.uid <= 0
        || payload.log_id <= 0
        || payload.email_hash.len() != 64
    {
        return Ok(Delivery::Skipped("sign-in alert payload is malformed"));
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
    if !preferences::locked(db, &mut tx, payload.uid)
        .await?
        .email_login
    {
        tx.commit().await?;
        return Ok(Delivery::Skipped("the recipient turned off sign-in alerts"));
    }
    let log = crate::db::query("SELECT uid,type,data,dateline FROM gonof_common_log WHERE id=?")
        .bind(payload.log_id)
        .fetch_optional(db.observe(&mut *tx))
        .await?;
    let Some(log) = log.filter(|row| {
        row.try_get::<Option<i64>, _>("uid").ok().flatten() == Some(payload.uid)
            && db::text(row, "type").ok().as_deref() == Some("identity.login")
    }) else {
        tx.commit().await?;
        return Ok(Delivery::Skipped("the sign-in record no longer exists"));
    };
    let data: serde_json::Value =
        serde_json::from_str(&db::text(&log, "data")?).unwrap_or_default();
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
    let dateline: i64 = log.try_get("dateline")?;
    hold_lease(
        db,
        &mut tx,
        mail,
        clock(),
        "the sign-in alert delivery lease expired",
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
    let ip = if ip.is_empty() {
        lang.mail_unknown().to_owned()
    } else {
        ip
    };
    let device = crate::identity::device_summary(lang, &user_agent);
    let time = templates::mail_time(dateline, &crate::clock::TimeFormat::from(&*site));
    let username = db::text(&member, "username")?;
    let link = crate::mail::site_link(&mailer.site_url().await?, "/my/account")?;
    let values = BTreeMap::from([
        ("username".into(), username.clone()),
        ("time".into(), time.clone()),
        ("ip".into(), ip.clone()),
        ("device".into(), device.clone()),
        ("link".into(), link.to_string()),
        ("site_name".into(), site_name.clone()),
    ]);
    let default_body = format!(
        "{}\r\n\r\n{}\r\n{}\r\n{}\r\n{}\r\n\r\n{}\r\n{link}\r\n\r\n{}\r\n{site_name}\r\n",
        lang.mail_greeting(username.as_str()),
        lang.mail_login_intro(),
        lang.mail_field_time(time.as_str()),
        lang.mail_field_ip(ip.as_str()),
        lang.mail_field_device(device.as_str()),
        lang.mail_login_advice(),
        lang.mail_login_optout()
    );
    let (subject, body) = templates::render_in(
        db,
        "login_alert",
        site.site.lang(),
        &values,
        lang.mail_login_subject(),
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
