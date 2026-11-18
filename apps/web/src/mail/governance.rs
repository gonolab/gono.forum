//! Sanction and appeal result mail does not depend on a login or an unconsumed identity token; the recipient and the
//! delivery lease must still be verified.
use super::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GovernanceMail {
    to: String,
    username: String,
    sanction_id: i64,
    appeal_id: Option<i64>,
    /// The reason the operator wrote (or the system's sentence for an expiry or an accepted appeal); the subject and
    /// the first line are worded from the event and the sanction when the mail is delivered (docs/architecture.md,
    /// "Languages").
    #[serde(default)]
    reason: String,
    /// Older queued mail carried the finished subject and sentence instead of `reason`; they are used only when
    /// `reason` is empty.
    #[serde(default)]
    subject: String,
    #[serde(default)]
    message: String,
}
pub(super) async fn deliver(
    mailer: &Mailer,
    settings: &settings::Stored,
    mail: &ClaimedMail,
    clock: &(impl Fn() -> i64 + Sync),
) -> Result<Delivery> {
    let Ok(payload) = serde_json::from_str::<GovernanceMail>(&mail.payload) else {
        return Ok(Delivery::Skipped("governance mail payload is malformed"));
    };
    if payload.sanction_id <= 0 {
        return Ok(Delivery::Skipped("governance mail payload is malformed"));
    }
    let db = &mailer.database;
    let mut tx = db.begin_write().await?;
    let current = crate::db::query(
        "SELECT email,emailstatus,account_state FROM gonof_common_member WHERE uid=? FOR UPDATE",
    )
    .bind(mail.aggregate_id)
    .fetch_optional(db.observe(&mut *tx))
    .await?;
    // Only a verified address gets the operator's reason (docs/roles.md, "Account sanctions"); mail queued before an
    // address lost its verification is dropped here.
    let eligible = match current {
        Some(member) => {
            db::text(&member, "account_state")? == "active"
                && member.try_get::<i64, _>("emailstatus")? == 1
                && member.try_get::<Option<String>, _>("email")?.as_deref()
                    == Some(payload.to.as_str())
                && !payload.to.is_empty()
        }
        None => false,
    };
    if !eligible {
        tx.commit().await?;
        return Ok(Delivery::Skipped(
            "the recipient account is deleted, or its email address changed or is unverified",
        ));
    }
    let sanction=crate::db::query("SELECT kind,reason,starts_at,expires_at,revoked_at,revoked_by FROM gonof_common_member_sanction WHERE id=? AND uid=? FOR UPDATE")
        .bind(payload.sanction_id).bind(mail.aggregate_id).fetch_optional(db.observe(&mut *tx)).await?;
    let Some(sanction) = sanction else {
        tx.commit().await?;
        return Ok(Delivery::Skipped("the sanction record does not exist"));
    };
    if mail.event_type == "sanction_ended" && sanction.try_get::<i64, _>("revoked_at")? == 0 {
        tx.commit().await?;
        return Ok(Delivery::Skipped("the sanction has not ended"));
    }
    let mut resolution = String::new();
    let mut appeal_accepted = false;
    if mail.event_type == "appeal_result" {
        let found=crate::db::query("SELECT status,resolution FROM gonof_common_member_appeal WHERE id=? AND sanction_id=? AND uid=? FOR UPDATE")
            .bind(payload.appeal_id).bind(payload.sanction_id).bind(mail.aggregate_id).fetch_optional(db.observe(&mut *tx)).await?;
        let Some(appeal) = found else {
            tx.commit().await?;
            return Ok(Delivery::Skipped("the appeal record does not exist"));
        };
        if !matches!(
            db::text(&appeal, "status")?.as_str(),
            "accepted" | "rejected"
        ) {
            tx.commit().await?;
            return Ok(Delivery::Skipped("the appeal has not been decided"));
        }
        resolution = db::text(&appeal, "resolution")?;
        appeal_accepted = db::text(&appeal, "status")? == "accepted";
    }
    hold_lease(
        db,
        &mut tx,
        mail,
        clock(),
        "the mail delivery lease expired; it will be claimed again",
    )
    .await?;
    // After the recheck and lease renewal, commit first, then render, write the draft to disk and send outside the lock
    // (on SQLite the whole site has a single writer, so a write transaction reads no templates and writes nothing to
    // disk). The lease is fully renewed, so no other deliverer can claim this mail; the draft is written to disk before
    // sending, and a disk write failure after commit is retried as usual.
    tx.commit().await?;
    let entry = crate::mail::site_link(&mailer.site_url().await?, "/appeals")?;
    let site_settings = mailer.site_settings().await?;
    let lang = site_settings.site.lang();
    let site = mailer.site_name(None).await?;
    // The worker has no request, so the site's time zone and date style come from the settings, as in the sign-in
    // alert; the page-time helper would fall back to its default zone here.
    let format = crate::clock::TimeFormat::from(&*site_settings);
    let expires: i64 = sanction.try_get("expires_at")?;
    let end = if expires == 0 {
        lang.mail_governance_no_end().into()
    } else {
        templates::mail_time(expires, &format)
    };
    let reason = db::text(&sanction, "reason")?;
    let starts_at = templates::mail_time(sanction.try_get("starts_at")?, &format);
    let kind = db::text(&sanction, "kind")?;
    let revoked = sanction.try_get::<Option<i64>, _>("revoked_by")?.is_some();
    // The operator's reason from the payload; older queued mail only had the finished sentence.
    let written = if payload.reason.is_empty() {
        payload.message.as_str()
    } else {
        payload.reason.as_str()
    };
    let (action, first) = match mail.event_type.as_str() {
        "sanction_issued" => match (kind.as_str(), expires) {
            ("warning", _) => (
                lang.mail_governance_warning(),
                lang.governance_notice_warning(written),
            ),
            ("mute", 0) => (
                lang.mail_governance_mute(),
                lang.governance_notice_mute_forever(written),
            ),
            ("mute", _) => (
                lang.mail_governance_mute(),
                lang.governance_notice_mute(written, end.as_str()),
            ),
            (_, 0) => (
                lang.mail_governance_freeze(),
                lang.governance_notice_freeze_forever(written),
            ),
            _ => (
                lang.mail_governance_freeze(),
                lang.governance_notice_freeze(written, end.as_str()),
            ),
        },
        "sanction_ended" => match (revoked, kind.as_str()) {
            (true, "freeze") => (
                lang.mail_governance_freeze_revoked(),
                lang.governance_notice_freeze_revoked(written),
            ),
            (true, "mute") => (
                lang.mail_governance_mute_revoked(),
                lang.governance_notice_mute_revoked(written),
            ),
            (true, _) => (
                lang.mail_governance_warning_revoked(),
                lang.governance_notice_warning_revoked(written),
            ),
            (false, "mute") => (
                lang.mail_governance_mute_expired(),
                lang.governance_notice_mute_expired(written),
            ),
            (false, _) => (
                lang.mail_governance_freeze_expired(),
                lang.governance_notice_freeze_expired(written),
            ),
        },
        _ if appeal_accepted => (
            lang.mail_governance_appeal_accepted(),
            lang.governance_notice_appeal_accepted(written),
        ),
        _ => (
            lang.mail_governance_appeal_rejected(),
            lang.governance_notice_appeal_rejected(written),
        ),
    };
    let subject = if payload.reason.is_empty() && !payload.subject.is_empty() {
        payload.subject.clone()
    } else {
        action.to_owned()
    };
    let first = if payload.reason.is_empty() {
        payload.message.clone()
    } else {
        first
    };
    // The appeal window and handling time follow the current settings; the built-in body and the
    // `{{appeal_window_days}}` and `{{appeal_handling_days}}` of a custom template use the same values.
    let governance = site_settings.governance.clone();
    let (window_days, handling_days) = (
        governance.appeal_window_days.to_string(),
        governance.appeal_handling_days.to_string(),
    );
    let default_body = format!(
        "{}\r\n\r\n{}\r\n\r\n{}\r\n{}\r\n{}\r\n{}\r\n\r\n{}\r\n{}\r\n{}\r\n{site}\r\n",
        lang.mail_greeting(payload.username.as_str()),
        first,
        lang.mail_governance_id(payload.sanction_id.to_string()),
        lang.mail_governance_reason(reason.as_str()),
        lang.mail_governance_starts(starts_at.as_str()),
        lang.mail_governance_ends(end.as_str()),
        lang.mail_governance_appeals(entry.to_string()),
        lang.mail_governance_appeal_rules(
            governance.appeal_window_days,
            governance.appeal_handling_days
        ),
        lang.mail_governance_note()
    );
    let values = BTreeMap::from([
        ("appeal_window_days".into(), window_days),
        ("appeal_handling_days".into(), handling_days),
        ("username".into(), payload.username.clone()),
        ("link".into(), entry.to_string()),
        ("reason".into(), reason),
        ("starts_at".into(), starts_at),
        ("expires_at".into(), end),
        ("resolution".into(), resolution),
        ("action".into(), subject.clone()),
        ("site_name".into(), site.clone()),
    ]);
    let (subject, body) =
        templates::render_in(db, &mail.event_type, lang, &values, &subject, &default_body).await?;
    let draft = mailer.prepared(mail, lang, &site, subject, body).await?;
    mailer
        .publish_prepared(settings, mail, &payload.to, &draft)
        .await?;
    confirm_sent(db, mail).await?;
    Ok(Delivery::Sent)
}
