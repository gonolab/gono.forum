use super::*;
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    message::{Mailbox, MultiPart, SinglePart, header::ContentType},
    transport::smtp::authentication::Credentials,
};

pub(super) fn message(
    settings: &MailSettings,
    to: &str,
    prefix: &str,
    subject: &str,
    body: String,
    id: &str,
) -> Result<Message> {
    compose(settings, to, prefix, subject, body, None, id, None)
}

pub(super) fn prepared_message(
    settings: &MailSettings,
    to: &str,
    draft: &PreparedMail,
    eventid: i64,
    dateline: i64,
) -> Result<Message> {
    compose(
        settings,
        to,
        &draft.subject_prefix,
        &draft.subject,
        draft.body.clone(),
        draft.html.as_deref(),
        &format!("outbox-{eventid}"),
        Some(dateline),
    )
}

#[allow(clippy::too_many_arguments)]
fn compose(
    settings: &MailSettings,
    to: &str,
    prefix: &str,
    subject: &str,
    body: String,
    html: Option<&str>,
    id: &str,
    dateline: Option<i64>,
) -> Result<Message> {
    let sender = settings::address(&settings.from_address)?;
    let domain = sender.domain().to_owned();
    let mut message = Message::builder()
        .from(Mailbox::new(Some(settings.from_name.clone()), sender))
        .to(Mailbox::new(None, settings::address(to)?))
        .subject(format!("{prefix}{subject}"))
        .message_id(Some(format!("<gonof-{id}@{domain}>")));
    if let Some(dateline) = dateline {
        ensure!(dateline >= 0, "invalid mail event time");
        message = message.date(
            std::time::UNIX_EPOCH
                .checked_add(std::time::Duration::from_secs(dateline as u64))
                .context("invalid mail event time")?,
        );
    }
    let message = if let Some(html) = html {
        message.multipart(
            MultiPart::alternative()
                .boundary(format!("gonof-{}", token_hash(id)))
                .singlepart(SinglePart::plain(body))
                .singlepart(SinglePart::html(html.to_owned())),
        )
    } else {
        message.header(ContentType::TEXT_PLAIN).body(body)
    };
    message.map_err(|_| anyhow::anyhow!("account mail could not be built"))
}

pub(super) fn local_message(to: &str, draft: &PreparedMail, eventid: i64) -> String {
    let mut rendered = format!(
        "To: {to}\r\nSubject: [gonof] {}\r\nMIME-Version: 1.0\r\n",
        draft.subject
    );
    if let Some(html) = &draft.html {
        // Base64 parts cannot contain this boundary; neither boundary nor body changes on retry.
        let boundary = format!("gonof-{}", token_hash(&format!("outbox-{eventid}")));
        rendered.push_str(&format!("Content-Type: multipart/alternative; boundary=\"{boundary}\"\r\nMessage-ID: <outbox-{eventid}@gonof.local>\r\n\r\n"));
        for (kind, body) in [("plain", draft.body.as_str()), ("html", html.as_str())] {
            rendered.push_str(&format!("--{boundary}\r\nContent-Type: text/{kind}; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n"));
            use base64::Engine;
            let encoded = base64::engine::general_purpose::STANDARD.encode(body.as_bytes());
            for line in encoded.as_bytes().chunks(76) {
                rendered.push_str(std::str::from_utf8(line).expect("base64 is ASCII"));
                rendered.push_str("\r\n");
            }
        }
        rendered.push_str(&format!("--{boundary}--\r\n"));
    } else {
        rendered.push_str(&format!("Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\nMessage-ID: <outbox-{eventid}@gonof.local>\r\n\r\n{}",draft.body));
    }
    rendered
}

impl Mailer {
    pub(super) async fn send_smtp(
        &self,
        stored: &settings::Stored,
        message: Message,
    ) -> Result<()> {
        let settings = &stored.settings;
        let password = settings::decrypt(&self.config, &stored.password_encrypted, settings)?;
        let builder = match settings.security.as_str() {
            "tls" => AsyncSmtpTransport::<Tokio1Executor>::relay(&settings.host),
            "starttls" => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&settings.host),
            _ => anyhow::bail!("SMTP encryption setting is not valid"),
        }
        .map_err(|_| anyhow::anyhow!("could not set up the encrypted SMTP connection"))?;
        let transport = builder
            .port(settings.port)
            .credentials(Credentials::new(settings.username.clone(), password))
            .timeout(Some(std::time::Duration::from_secs(10)))
            .build();
        // Bound the complete operation as well as each SMTP command, below the lease lifetime.
        tokio::time::timeout(std::time::Duration::from_secs(15), transport.send(message))
            .await
            .map_err(|_| {
                anyhow::anyhow!("SMTP delivery timed out and will be retried; check that the server is reachable")
            })?
            // Provider replies may contain recipients, credentials or message fragments.
            .map_err(|error| {
                if error.is_timeout() {
                    anyhow::anyhow!("SMTP connection timed out; check the server and port")
                } else if error.is_permanent() {
                    let code = error
                        .status()
                        .map(|code| code.to_string())
                        .unwrap_or_default();
                    let reply = std::error::Error::source(&error)
                        .map(ToString::to_string)
                        .unwrap_or_default();
                    // Only the enhanced status code is kept as a marker; the server's raw text is neither stored nor
                    // logged.
                    match recipient_rejected(&reply) {
                        Some(enhanced) => anyhow::Error::new(Rejected(format!(
                            "the receiving server rejected this mailbox ({code} {enhanced})"
                        ))),
                        None => anyhow::anyhow!(
                            "the SMTP server refused delivery ({code}); check the account, authorization code, sender and recipient addresses"
                        ),
                    }
                } else {
                    anyhow::anyhow!(
                        "SMTP delivery failed; check the encryption, certificate and server connection. Queued mail is retried automatically"
                    )
                }
            })?;
        Ok(())
    }
}

/// Whether the server reply shows that this recipient mailbox itself cannot be delivered to.
///
/// Only recognizes the RFC 3463 enhanced status codes that point at the recipient mailbox: mailbox does not exist
/// (5.1.1), invalid domain (5.1.2, 5.1.10), bad address syntax (5.1.3), mailbox moved (5.1.6), mailbox disabled
/// (5.2.1). lettre does not tell us whether MAIL FROM, RCPT TO or DATA was rejected; a 550/553 without an enhanced
/// status code may be a sender or policy problem that can still be sent once the configuration is fixed, so it is
/// retried as a temporary failure.
fn recipient_rejected(reply: &str) -> Option<&str> {
    let code = reply.split_whitespace().next()?;
    matches!(
        code,
        "5.1.1" | "5.1.2" | "5.1.3" | "5.1.6" | "5.1.10" | "5.2.1"
    )
    .then_some(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_recipient_specific_enhanced_codes_are_permanent() {
        for reply in [
            "5.1.1 The email account that you tried to reach does not exist.",
            "5.1.10 Recipient address has null MX",
            "5.2.1 mailbox disabled\n5.2.1 second line",
        ] {
            assert!(recipient_rejected(reply).is_some(), "{reply}");
        }
        for reply in [
            "",
            "Mailbox not found",
            "5.7.8 Authentication credentials invalid",
            "5.7.1 Sender address rejected: not owned by user",
            "5.1.7 Bad sender's mailbox address syntax",
            "5.3.4 Message size exceeds fixed limit",
            "4.2.2 Mailbox full",
        ] {
            assert!(recipient_rejected(reply).is_none(), "{reply}");
        }
    }

    #[test]
    fn prepared_multipart_has_stable_bytes_and_a_plain_text_alternative() {
        let draft = PreparedMail {
            subject_prefix: "[Sample Community] ".into(),
            subject: "Verify your email".into(),
            body: "Hello, world\nhttps://forum.example.test/link".into(),
            content_kind: None,
            html: Some(branding::html(
                crate::i18n::Lang::EN,
                "Sample Community",
                "Hello, world",
                "https://forum.example.test/brand/email/12/source",
            )),
        };
        let settings = MailSettings {
            from_address: "sender@example.test".into(),
            from_name: "Sample Forum".into(),
            ..Default::default()
        };
        let smtp = prepared_message(&settings, "reader@example.test", &draft, 42, 1700000000)
            .unwrap()
            .formatted();
        assert_eq!(
            smtp,
            prepared_message(&settings, "reader@example.test", &draft, 42, 1700000000)
                .unwrap()
                .formatted()
        );
        let text = String::from_utf8(smtp).unwrap();
        assert!(text.contains("multipart/alternative"));
        assert!(text.contains("text/plain"));
        assert!(text.contains("text/html"));
        assert!(text.contains("gonof-outbox-42@example.test"));
        let local = local_message("reader@example.test", &draft, 42);
        assert_eq!(local, local_message("reader@example.test", &draft, 42));
        assert!(local.contains("multipart/alternative"));
        assert_eq!(
            local.matches("Content-Transfer-Encoding: base64").count(),
            2
        );
        let plain = PreparedMail {
            html: None,
            ..draft
        };
        let local = local_message("reader@example.test", &plain, 43);
        assert!(local.contains("Content-Type: text/plain"));
        assert!(!local.contains("multipart"));
        assert!(local.contains("Hello, world"));
    }
}
