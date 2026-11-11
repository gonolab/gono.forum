//! Optional mail artwork uses the public site route and never enables HTML templates.
use super::*;

pub(super) async fn current_url(db: &Database, site_url: &str) -> Result<Option<String>> {
    let row=db::query("SELECT s.aid,d.lifecycle,d.scan_status,d.mime_type FROM gonof_common_site_asset s JOIN gonof_forum_attachment_0 d ON d.aid=s.aid WHERE s.slot='email'")
        .fetch_optional(db.executor()).await?;
    let Some(row) = row else { return Ok(None) };
    if db::text(&row, "lifecycle")? != "ready"
        || db::text(&row, "scan_status")? != "clean"
        || !db::text(&row, "mime_type")?.starts_with("image/")
    {
        return Ok(None);
    }
    let aid: i64 = row.try_get("aid")?;
    let url = crate::mail::site_link(site_url, &format!("/brand/email/{aid}/source"))?;
    Ok(Some(url.to_string()))
}

fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// The HTML part of a mail: the brand image (its text alternative is the site name) and the body; the language is the
/// mail's (docs/architecture.md, "Languages").
pub(super) fn html(lang: crate::i18n::Lang, site: &str, body: &str, image_url: &str) -> String {
    format!(
        "<!doctype html><html lang=\"{}\"><head><meta charset=\"utf-8\"></head><body><img src=\"{}\" alt=\"{}\" width=\"240\" style=\"display:block;max-width:100%;height:auto\"><pre style=\"font-family:system-ui,sans-serif;white-space:pre-wrap;overflow-wrap:anywhere\">{}</pre></body></html>",
        lang.tag(),
        escape(image_url),
        escape(site),
        escape(body)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn html_uses_only_escaped_plain_text_and_the_chosen_site_image() {
        let output = html(
            crate::i18n::Lang::EN,
            "Probe <Forum>",
            "<script>alert('x')</script>\n<img src=\"https://example.org/private\"> & body",
            "https://forum.example.test/brand/email/12/source",
        );
        assert!(output.contains("&lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt;"));
        assert!(output.contains("&amp; body"));
        assert!(!output.contains("<script>"));
        assert_eq!(output.matches("<img ").count(), 1);
        assert!(output.contains("src=\"https://forum.example.test/brand/email/12/source\""));
    }
}
