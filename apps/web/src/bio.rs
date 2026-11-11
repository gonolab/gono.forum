//! Link rendering in member bios.
//!
//! Bios are still stored as plain text; rendering escapes the whole text and only wraps recognised addresses in links:
//! - Only addresses starting with `http://` / `https://` count, and the host must be a dotted domain name; IP
//!   literals, `user:pass@` and single-label host names (`localhost`, short intranet names) always stay text.
//! - An address ends at whitespace, a non-ASCII character or `<>"'` (Chinese text right after an address is
//!   common); trailing `.,;:!?` and an unpaired closing parenthesis are not part of the address.
//! - Only the first 3 become links; the rest stay text.
//! - The original text is shown, elided in the middle past 48 characters, with the full address in `title`;
//!   `href` uses the parsed canonical form.
//! - `rel="nofollow ugc noopener noreferrer" target="_blank"`, the same for every member's bio.
//! - Links carry `translate="no"`: the original address is kept when the browser translates the page.
//!
//! Used only in the profile header of `/space/:uid` and the `/my` overview; summaries such as member search results
//! stay plain text.
/// The same set of entities as Askama's template escaping: bios are output with `|safe`, and a bio without addresses
/// comes out byte-for-byte as template escaping would have produced it.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '<' => out.push_str("&#60;"),
            '>' => out.push_str("&#62;"),
            '&' => out.push_str("&#38;"),
            '"' => out.push_str("&#34;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

pub const MAX_LINKS: usize = 3;
const DISPLAY_CHARS: usize = 48;

/// The escaped bio HTML, ready for `|safe` output.
pub fn html(bio: &str) -> String {
    let mut out = String::with_capacity(bio.len() + 64);
    let mut rest = bio;
    let mut links = 0;
    while let Some(start) = scheme_start(rest) {
        out.push_str(&escape(&rest[..start]));
        let candidate = &rest[start..];
        let end = url_end(candidate);
        let raw = &candidate[..end];
        match accept(raw).filter(|_| links < MAX_LINKS) {
            Some(href) => {
                links += 1;
                out.push_str(&format!(
                    "<a href=\"{}\" rel=\"nofollow ugc noopener noreferrer\" target=\"_blank\" translate=\"no\" title=\"{}\">{}</a>",
                    escape(&href),
                    escape(raw),
                    escape(&display(raw))
                ));
            }
            None => out.push_str(&escape(raw)),
        }
        rest = &candidate[end..];
    }
    out.push_str(&escape(rest));
    out
}

/// Position of the next `http://` or `https://` (case-insensitive); it does not count when the preceding character is
/// a letter or digit (`xhttp://` is not the start of an address).
fn scheme_start(text: &str) -> Option<usize> {
    let lower = text.to_ascii_lowercase();
    let mut from = 0;
    while let Some(offset) = lower[from..].find("http") {
        let index = from + offset;
        let tail = &lower[index..];
        let boundary = text[..index]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_ascii_alphanumeric());
        if boundary && (tail.starts_with("http://") || tail.starts_with("https://")) {
            return Some(index);
        }
        from = index + 4;
    }
    None
}

fn url_end(candidate: &str) -> usize {
    let mut end = candidate
        .char_indices()
        .find(|(_, c)| !c.is_ascii_graphic() || matches!(c, '<' | '>' | '"' | '\'' | '`'))
        .map_or(candidate.len(), |(index, _)| index);
    loop {
        let url = &candidate[..end];
        let Some(last) = url.chars().next_back() else {
            break;
        };
        let unbalanced = match last {
            ')' => url.matches(')').count() > url.matches('(').count(),
            ']' => url.matches(']').count() > url.matches('[').count(),
            '.' | ',' | ';' | ':' | '!' | '?' => true,
            _ => false,
        };
        if !unbalanced {
            break;
        }
        end -= last.len_utf8();
    }
    end
}

/// For a valid address, returns the normalised `href`.
fn accept(raw: &str) -> Option<String> {
    let url = url::Url::parse(raw).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    match url.host()? {
        url::Host::Domain(domain) if domain.contains('.') && !domain.ends_with('.') => {}
        _ => return None,
    }
    Some(url.to_string())
}

fn display(raw: &str) -> String {
    let count = raw.chars().count();
    if count <= DISPLAY_CHARS {
        return raw.to_owned();
    }
    let head: String = raw.chars().take(30).collect();
    let tail: String = raw.chars().skip(count - (DISPLAY_CHARS - 31)).collect();
    format!("{head}…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const REL: &str = "rel=\"nofollow ugc noopener noreferrer\" target=\"_blank\" translate=\"no\"";

    #[test]
    fn links_get_the_agreed_attributes_and_text_stays_escaped() {
        // Chinese text may touch a link on both sides: "\u{535a}\u{5ba2}" is the Chinese for "blog" and "\u{ff0c}" is
        // a full-width comma.
        assert_eq!(
            html("\u{535a}\u{5ba2}https://blog.example.com/a?b=1&c=2\u{ff0c} come and see <b>"),
            format!(
                "\u{535a}\u{5ba2}<a href=\"https://blog.example.com/a?b=1&#38;c=2\" {REL} title=\"https://blog.example.com/a?b=1&#38;c=2\">https://blog.example.com/a?b=1&#38;c=2</a>\u{ff0c} come and see &#60;b&#62;"
            )
        );
        // Sentence-final punctuation and unpaired parentheses are not part of the address.
        assert!(html("(see https://a.example.org/x).").ends_with("</a>)."));
        assert!(
            html("https://en.example.org/wiki/A_(b)")
                .contains("title=\"https://en.example.org/wiki/A_(b)\"")
        );
        // Uppercase schemes are recognised too; href uses the canonical form.
        assert!(html("HTTPS://Example.COM/Path").contains("href=\"https://example.com/Path\""));
    }

    #[test]
    fn only_domain_hosts_become_links() {
        for text in [
            "http://127.0.0.1/admin",
            "http://[::1]/",
            "http://0x7f.1/",
            "http://localhost:8080/",
            "http://user:pass@example.com/",
            "ftp://example.com/",
            "javascript:alert(1)",
            "xhttps://example.com/",
            "https://",
        ] {
            assert!(!html(text).contains("<a "), "{text}");
            assert!(!html(text).contains('<') || text.contains('<'), "{text}");
        }
    }

    #[test]
    fn at_most_three_links_and_long_ones_are_shortened() {
        let text = "https://a.example.com https://b.example.com https://c.example.com https://d.example.com";
        let out = html(text);
        assert_eq!(out.matches("<a ").count(), MAX_LINKS);
        assert!(out.ends_with(" https://d.example.com"));
        let long = format!("https://example.com/{}", "x".repeat(80));
        let out = html(&long);
        assert!(out.contains(&format!("title=\"{long}\"")));
        let shown = out.split('>').nth(1).unwrap().trim_end_matches("</a");
        assert_eq!(shown.chars().count(), DISPLAY_CHARS);
        assert!(shown.contains('…'));
    }

    #[test]
    fn plain_bios_are_only_escaped() {
        assert_eq!(
            html("First line\nSecond line & \"quotes\""),
            "First line\nSecond line &#38; &#34;quotes&#34;"
        );
    }
}
