//! Plain-text e-mail templates. Each kind keeps one row per language in `gonof_common_localized_text` (kind
//! `mail.{kind}`, docs/architecture.md, "Languages"): the title column is the subject. Saving overwrites the row
//! and no history is kept; "restore built-in text" deletes it. The variable language is deliberately kept very small.
use crate::{
    db::{self, Database},
    i18n::Lang,
    identity::Actor,
};
use anyhow::{Context, Result, ensure};
use std::collections::BTreeMap;

/// The name of a template kind in a language: a generated method of [`Lang`].
pub type KindLabel = fn(Lang) -> &'static str;

/// Every template kind and its name (a translation key, mail.ftl).
pub const KINDS: &[(&str, KindLabel)] = &[
    ("email_verification", Lang::mail_kind_email_verification),
    ("password_reset", Lang::mail_kind_password_reset),
    ("email_change", Lang::mail_kind_email_change),
    ("appeal_access", Lang::mail_kind_appeal_access),
    ("privacy_access", Lang::mail_kind_privacy_access),
    ("sanction_issued", Lang::mail_kind_sanction_issued),
    ("sanction_ended", Lang::mail_kind_sanction_ended),
    ("appeal_result", Lang::mail_kind_appeal_result),
    ("content", Lang::mail_kind_content),
    ("message", Lang::mail_kind_message),
    ("login_alert", Lang::mail_kind_login_alert),
    ("security_notice", Lang::mail_kind_security_notice),
];
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MailTemplate {
    pub subject: String,
    pub body: String,
}
impl MailTemplate {
    /// Subject and body both blank means "not customized", and the built-in text is used. With the revision number
    /// gone, this state expresses what used to be `revision == 0`, and "Restore built-in text" has always meant
    /// clearing these two fields.
    ///
    /// `trim` rather than `is_empty`: if a body made only of spaces counted as a custom template, it would fail
    /// validation at delivery time, and one formatting slip would keep this kind of e-mail from ever being sent.
    fn builtin(&self) -> bool {
        self.subject.trim().is_empty() && self.body.trim().is_empty()
    }
}
/// One template in one language: a primary key lookup.
const READ_SQL: &str =
    "SELECT title,body FROM gonof_common_localized_text WHERE kind=? AND language=?";
/// The kind column of a template: `mail.{kind}`.
fn row_kind(kind: &str) -> Result<String> {
    ensure!(
        KINDS.iter().any(|(value, _)| *value == kind),
        crate::i18n::current().mail_templates_error_kind()
    );
    Ok(format!("mail.{kind}"))
}
pub fn variables(kind: &str) -> &'static [&'static str] {
    match kind {
        "content" => &["username", "title", "link", "action", "site_name"],
        // The two appeal day counts come from the current settings `settings.governance.appeal_window_days` and
        // `appeal_handling_days`.
        "sanction_issued" | "sanction_ended" | "appeal_result" => &[
            "username",
            "link",
            "reason",
            "starts_at",
            "expires_at",
            "resolution",
            "action",
            "site_name",
            "appeal_window_days",
            "appeal_handling_days",
        ],
        "message" => &["username", "sender", "count", "link", "site_name"],
        "login_alert" => &["username", "time", "ip", "device", "link", "site_name"],
        "security_notice" => &[
            "username",
            "event",
            "time",
            "ip",
            "device",
            "link",
            "site_name",
        ],
        _ => &["username", "link", "site_name"],
    }
}
fn expand(input: &str, values: &BTreeMap<String, String>) -> Result<String> {
    let mut output = String::new();
    let mut rest = input;
    while let Some(start) = rest.find("{{") {
        ensure!(
            !rest[..start].contains("}}"),
            crate::i18n::current().mail_templates_error_marker()
        );
        output.push_str(&rest[..start]);
        let part = &rest[start + 2..];
        let end = part
            .find("}}")
            .context(crate::i18n::current().mail_templates_error_unclosed())?;
        let name = part[..end].trim();
        output.push_str(
            values
                .get(name)
                .context(crate::i18n::current().mail_templates_error_unknown())?,
        );
        rest = &part[end + 2..];
    }
    ensure!(
        !rest.contains("}}"),
        crate::i18n::current().mail_templates_error_marker()
    );
    output.push_str(rest);
    Ok(output)
}
pub fn validate(kind: &str, value: &MailTemplate) -> Result<()> {
    row_kind(kind)?;
    ensure!(
        !value.subject.contains(['\r', '\n']) && value.subject.chars().count() <= 120,
        crate::i18n::current().mail_templates_error_subject()
    );
    ensure!(
        !value.subject.trim().is_empty()
            && !value.body.trim().is_empty()
            && value.body.len() <= 16000,
        crate::i18n::current().mail_templates_error_required()
    );
    ensure!(
        value.body.split("{{").skip(1).any(|part| part
            .split_once("}}")
            .is_some_and(|(name, _)| name.trim() == "link")),
        crate::i18n::current().mail_templates_error_link()
    );
    let values = variables(kind)
        .iter()
        .map(|key| (key.to_string(), "preview".into()))
        .collect();
    expand(&value.subject, &values)?;
    expand(&value.body, &values)?;
    Ok(())
}
/// The site language, for the calls that do not name a language.
async fn site_language(db: &Database) -> Result<crate::i18n::Lang> {
    Ok(crate::runtime::read(db)
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?
        .site
        .lang())
}

/// The current template in the site language (see [`current_in`]).
pub async fn current(db: &Database, kind: &str) -> Result<MailTemplate> {
    current_in(db, kind, site_language(db).await?).await
}

/// Fetches the current template in one language; none saved means the built-in text (both fields empty). Subject
/// and body are one row, so a save can never be read as "new subject + old body".
pub async fn current_in(
    db: &Database,
    kind: &str,
    lang: crate::i18n::Lang,
) -> Result<MailTemplate> {
    let row = db::query(READ_SQL)
        .bind(row_kind(kind)?)
        .bind(lang.tag())
        .fetch_optional(db.executor())
        .await?;
    Ok(match row {
        Some(row) => MailTemplate {
            subject: db::text(&row, "title")?,
            body: db::text(&row, "body")?,
        },
        None => MailTemplate::default(),
    })
}
/// Saves one template in the site language (see [`save_in`]).
pub async fn save(
    db: &Database,
    actor: Actor,
    kind: &str,
    value: MailTemplate,
    reset: bool,
) -> Result<MailTemplate> {
    save_in(db, actor, kind, site_language(db).await?, value, reset).await
}

/// Saves one template in one language; `reset` deletes the row so that language uses its built-in text again. The
/// twelve templates and their languages are unrelated rows, so saves never wait on each other.
pub async fn save_in(
    db: &Database,
    actor: Actor,
    kind: &str,
    lang: crate::i18n::Lang,
    mut value: MailTemplate,
    reset: bool,
) -> Result<MailTemplate> {
    let row_kind = row_kind(kind)?;
    value.body = crate::runtime::form_lines(&value.body);
    if !reset {
        validate(kind, &value)?;
    }
    if reset {
        value.subject.clear();
        value.body.clear();
    }
    let mut tx = db.begin_write().await?;
    crate::runtime::admin_locked(db, &mut tx, actor).await?;
    if reset {
        db::query("DELETE FROM gonof_common_localized_text WHERE kind=? AND language=?")
            .bind(&row_kind)
            .bind(lang.tag())
            .execute(db.observe(&mut *tx))
            .await?;
    } else {
        db::query("INSERT INTO gonof_common_localized_text(kind,language,title,body,updated_at) VALUES(?,?,?,?,?) ON DUPLICATE KEY UPDATE title=VALUES(title),body=VALUES(body),updated_at=VALUES(updated_at)")
            .bind(&row_kind)
            .bind(lang.tag())
            .bind(&value.subject)
            .bind(&value.body)
            .bind(crate::identity::now())
            .execute(db.observe(&mut *tx))
            .await?;
    }
    crate::runtime::audit(
        db,
        &mut tx,
        actor,
        "mail.template.save",
        serde_json::json!({"template":kind,"language":lang.tag(),"reset":reset}),
    )
    .await?;
    tx.commit().await?;
    Ok(value)
}
/// Renders a pending e-mail.
///
/// With history snapshots removed, this reads the **current** template: the moment the text is finalized moves
/// from "inside the enqueue transaction" to "at the first delivery attempt". After the first delivery succeeds,
/// retries still reuse the prepared message `mail/prepared/{eventid}.eml` with identical bytes; that guarantee
/// rests on the prepared file, not on a database snapshot.
///
/// The cost is that one invalid template blocks every pending e-mail of its kind (render failure → transaction Err →
/// back to pending with exponential backoff); the operator's lever is "Restore built-in text" in the admin area.
/// This renders in the site language; delivery passes the mail's own language to [`render_in`].
pub async fn render(
    db: &Database,
    kind: &str,
    values: &BTreeMap<String, String>,
    default_subject: &str,
    default_body: &str,
) -> Result<(String, String)> {
    let lang = site_language(db).await?;
    render_in(db, kind, lang, values, default_subject, default_body).await
}

/// Renders a pending e-mail in `lang`, the mail's language (docs/architecture.md, "Languages"): the starting
/// domain's for mail a member started, the site's for background notifications.
pub async fn render_in(
    db: &Database,
    kind: &str,
    lang: crate::i18n::Lang,
    values: &BTreeMap<String, String>,
    default_subject: &str,
    default_body: &str,
) -> Result<(String, String)> {
    row_kind(kind)?;
    // Built-in text can also have line breaks spliced in by the caller (the sanction notice's subject comes from
    // the payload), so the header-injection defense must not cover only custom templates.
    ensure!(
        !default_subject.contains(['\r', '\n']),
        "mail subject must not contain line breaks"
    );
    let template = current_in(db, kind, lang).await?;
    if template.builtin() {
        return Ok((default_subject.into(), default_body.into()));
    }
    // Existing dirty data (hand-edited database, or tightened validation rules) must not slip through silently.
    validate(kind, &template)?;
    expanded(&template, values)
}
fn expanded(
    template: &MailTemplate,
    values: &BTreeMap<String, String>,
) -> Result<(String, String)> {
    let subject = expand(&template.subject, values)?;
    ensure!(
        !subject.contains(['\r', '\n']),
        crate::i18n::current().mail_templates_error_subject_variable()
    );
    Ok((subject, expand(&template.body, values)?))
}
/// A point in time in e-mail: the full time in the site time zone with the offset, such as
/// "2026-10-31 20:15 (UTC+8)". No relative wording: e-mail may be read much later. Delivery (login alerts,
/// security notices) and the template preview in the admin area share this one format.
pub fn mail_time(timestamp: i64, format: &crate::clock::TimeFormat) -> String {
    let format = crate::clock::TimeFormat {
        relative_hours: None,
        ..*format
    };
    format.lang.mail_time(
        crate::clock::datetime(timestamp, &format),
        crate::clock::zone_label(format.offset_minutes),
    )
}

/// Sample time for `{{time}}` in the preview: 2026-10-31 12:15 UTC, shown as 20:15 on a UTC+8 site.
const PREVIEW_TIME: i64 = 1_793_448_900;

/// `format` is this request's site time convention ([`crate::clock::current`]): the sample time is written in the
/// site time zone, consistent with real e-mail.
pub fn preview(
    kind: &str,
    value: &MailTemplate,
    site_url: &str,
    format: &crate::clock::TimeFormat,
) -> Result<(String, String)> {
    validate(kind, value)?;
    let t = format.lang;
    let values = variables(kind)
        .iter()
        .map(|key| {
            (
                key.to_string(),
                match *key {
                    "username" => t.mail_preview_username().into(),
                    "sender" => t.mail_preview_sender().into(),
                    "count" => "3".into(),
                    "time" => mail_time(PREVIEW_TIME, format),
                    "ip" => "203.0.113.7".into(),
                    "device" => "Chrome · macOS".into(),
                    "title" => t.mail_preview_title().into(),
                    "site_name" => t.mail_preview_site_name().into(),
                    "appeal_window_days" => "30".into(),
                    "appeal_handling_days" => "7".into(),
                    "link" => format!("{}/", site_url.trim_end_matches('/')),
                    _ => t.mail_preview_other().into(),
                },
            )
        })
        .collect();
    expanded(value, &values)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn templates_reject_code_unknown_variables_and_header_injection() {
        let mut value = MailTemplate {
            subject: "Hello {{username}}".into(),
            body: "Please open {{link}}".into(),
        };
        assert!(validate("content", &value).is_ok());
        value.body = "{{link}} {{secret}}".into();
        assert!(validate("content", &value).is_err());
        value.body = "No link".into();
        assert!(validate("content", &value).is_err());
        value.body = "{{link}}".into();
        value.subject = "Subject\nBcc: other".into();
        assert!(validate("content", &value).is_err());
    }
    /// Full time in the site time zone with the offset, never a fixed zone name; even with relative times enabled, no
    /// "N minutes ago".
    #[test]
    fn mail_times_follow_the_site_zone_and_never_go_relative() {
        use crate::clock::{DateStyle, TimeFormat};
        let zone = |offset_minutes| TimeFormat {
            lang: crate::i18n::Lang::EN,
            offset_minutes,
            date: DateStyle::YmdDash,
            clock24: true,
            relative_hours: Some(720),
        };
        for (offset, text) in [
            (480, "2026-10-31 20:15 (UTC+8)"),
            (330, "2026-10-31 17:45 (UTC+5:30)"),
            (-180, "2026-10-31 09:15 (UTC-3)"),
            (0, "2026-10-31 12:15 (UTC)"),
        ] {
            assert_eq!(mail_time(PREVIEW_TIME, &zone(offset)), text);
        }
        let now = crate::identity::now();
        assert!(mail_time(now - 60, &zone(480)).ends_with(" (UTC+8)"));
        let value = MailTemplate {
            subject: "New sign-in".into(),
            body: "Time: {{time}} {{link}}".into(),
        };
        let (_, body) = preview("login_alert", &value, "https://example.test", &zone(0)).unwrap();
        assert_eq!(body, "Time: 2026-10-31 12:15 (UTC) https://example.test/");
    }
    #[test]
    fn templates_parse_variables_once_and_reject_unbalanced_markers() {
        let mut value = MailTemplate {
            subject: "Hello {{username}}".into(),
            body: "Please open {{  link  }}".into(),
        };
        assert!(validate("privacy_access", &value).is_ok());
        let mut values = BTreeMap::from([
            ("username".into(), "{{secret}}".into()),
            (
                "link".into(),
                "https://example.test/data/access?token=synthetic".into(),
            ),
        ]);
        assert_eq!(expanded(&value, &values).unwrap().0, "Hello {{secret}}");
        values.insert("username".into(), "name\r\nBcc: other".into());
        assert!(expanded(&value, &values).is_err());
        for body in ["}} {{link}}", "{{link}} {{username", "{{{{link}}"] {
            value.body = body.into();
            assert!(validate("privacy_access", &value).is_err());
        }
        assert!(validate("unsupported", &value).is_err());
    }
}
