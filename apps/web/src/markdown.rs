//! Shared Markdown interpretation for previews, published posts and attachment binding.
//!
//! Rendering bounds how deeply a body can nest. The sanitizer's HTML parser walks the open elements on many start
//! tags, so a body that nests all the way down takes time that grows with the square of its length. Markdown
//! elements deeper than [`MAX_NESTING`] levels are flattened, and raw HTML after the first [`MAX_HTML_TAGS`] start
//! tags is shown as text ([`bounded_events`]).
use crate::db::Database;
use anyhow::{Result, ensure};
use pulldown_cmark::{CowStr, Event, Options, Parser, Tag, TagEnd, html};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default, Debug)]
pub struct References {
    pub attachments: BTreeSet<i64>,
    pub members: BTreeSet<i64>,
    pub quotes: BTreeSet<i64>,
}

/// Maximum number of attachments one piece of content may reference. The publishing setting `max_attachments` can only
/// be set between 1 and this number.
pub const MAX_ATTACHMENTS: usize = 20;

/// Task lists are not enabled: the sanitizer does not allow `input`, so rendered checkboxes would be stripped entirely
/// and `- [ ] todo` would lose its brackets too. Neither the editor nor the docs offer this syntax, and showing the
/// original text is simpler than allowing user-writable `<input>`.
fn options() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH
}

/// How deeply Markdown elements (block quotes, lists and their items, emphasis, links, …) nest in rendered HTML.
/// Real posts stay within a handful of levels; without a limit, a body of 60,000 `>` (one block quote inside the
/// next) keeps a request's worker thread in the sanitizer for over 30 seconds in a release build.
pub const MAX_NESTING: usize = 32;

/// How many raw HTML start tags one body may use; all raw HTML after them is shown as text. Raw HTML nests outside
/// Markdown's structure, so this bounds the sanitizer's work the same way: what is left grows with this number
/// times the body's start tags. Each spoiler uses two.
pub const MAX_HTML_TAGS: usize = 128;

/// The events of a body, after [`spoilers`], bounded for the sanitizer. Every path that hands parsed Markdown to the
/// sanitizer goes through here.
///
/// - A Markdown element deeper than [`MAX_NESTING`] is dropped, start and end; its content stays, inside the deepest
///   element that was kept.
/// - Raw HTML is passed through while the body's raw HTML start tags stay within [`MAX_HTML_TAGS`]. From the piece
///   that would exceed them on, every piece of raw HTML is text, so end tags are shown as well; a spoiler wrapper
///   shows as the `<spoiler>` the member typed.
fn bounded_events<'a>(source: &'a str) -> Vec<Event<'a>> {
    let open = spoiler_open();
    let mut tags = 0_usize;
    let mut as_text = false;
    // `Ok` keeps a piece of raw HTML, `Err` is the text to show instead.
    let mut raw = |value: CowStr<'a>| -> Result<CowStr<'a>, CowStr<'a>> {
        if !as_text {
            let count = start_tags(&value);
            if tags + count <= MAX_HTML_TAGS {
                tags += count;
                return Ok(value);
            }
            as_text = true;
        }
        Err(value
            .replace(&open, "<spoiler>")
            .replace(SPOILER_CLOSE, "</spoiler>")
            .into())
    };
    let mut depth = 0_usize;
    let mut events = Vec::new();
    for event in Parser::new_ext(source, options()) {
        events.push(match event {
            Event::Start(tag) => {
                depth += 1;
                if depth > MAX_NESTING {
                    continue;
                }
                Event::Start(tag)
            }
            // Start and end events are balanced, so an end sees the depth its start had.
            Event::End(tag) => {
                depth = depth.saturating_sub(1);
                if depth >= MAX_NESTING {
                    continue;
                }
                Event::End(tag)
            }
            Event::Html(value) => raw(value).map_or_else(Event::Text, Event::Html),
            Event::InlineHtml(value) => raw(value).map_or_else(Event::Text, Event::InlineHtml),
            event => event,
        });
    }
    events
}

/// Start tags in a piece of raw HTML, counted as `<` followed by a letter. The tokenizer starts a tag at each of them
/// outside comments and attribute values, and counting those as well only makes the number larger.
fn start_tags(html: &str) -> usize {
    html.as_bytes()
        .windows(2)
        .filter(|pair| pair[0] == b'<' && pair[1].is_ascii_alphabetic())
        .count()
}
fn reference(url: &str, prefix: &str) -> Result<Option<i64>> {
    let Some(id) = url.strip_prefix(prefix) else {
        return Ok(None);
    };
    let value = id.parse::<i64>().ok().filter(|v| *v > 0);
    ensure!(
        value.is_some() && id.bytes().all(|b| b.is_ascii_digit()),
        "{}",
        crate::i18n::current().editor_error_reference()
    );
    Ok(value)
}
pub fn references(source: &str) -> Result<References> {
    let source = spoilers(source);
    let mut result = References::default();
    let mut image_depth = 0;
    for event in Parser::new_ext(&source, options()) {
        match event {
            Event::Start(Tag::Image { dest_url, .. }) => {
                image_depth += 1;
                if let Some(id) = reference(&dest_url, "attachment:")? {
                    result.attachments.insert(id);
                }
            }
            Event::End(TagEnd::Image) => image_depth -= 1,
            Event::Start(Tag::Link { dest_url, .. }) => {
                if let Some(id) = reference(&dest_url, "attachment:")? {
                    result.attachments.insert(id);
                }
                if image_depth == 0 {
                    if let Some(id) = reference(&dest_url, "member:")? {
                        result.members.insert(id);
                    }
                    if let Some(id) = reference(&dest_url, "post:")? {
                        result.quotes.insert(id);
                    }
                }
            }
            _ => {}
        }
    }
    ensure!(
        result.attachments.len() <= MAX_ATTACHMENTS,
        "{}",
        crate::i18n::current().editor_error_attachments(MAX_ATTACHMENTS)
    );
    ensure!(
        result.members.len() <= 50,
        "{}",
        crate::i18n::current().editor_error_mentions(50)
    );
    ensure!(
        result.quotes.len() <= 50,
        "{}",
        crate::i18n::current().editor_error_quotes(50)
    );
    Ok(result)
}

/// Candidates for list thumbnails: image attachments `![…](attachment:N)` in the body in order of appearance,
/// deduplicated. Only image syntax counts, not links like `[file](attachment:N)`; those inside spoilers do not count,
/// since lists must not reveal spoilers. Whether it is a static image and whether it is bound to this post is decided
/// by the caller from the attachment rows (`content::thread_images`).
pub fn image_attachments(source: &str) -> Vec<i64> {
    let source = spoilers(source);
    let mut spoiler_depth = 0_usize;
    let mut found = Vec::new();
    for event in Parser::new_ext(&source, options()) {
        match event {
            Event::Html(value) | Event::InlineHtml(value) => {
                spoiler_depth += value.matches("<details class=\"reveal\">").count();
                spoiler_depth = spoiler_depth.saturating_sub(value.matches("</details>").count());
            }
            Event::Start(Tag::Image { dest_url, .. }) if spoiler_depth == 0 => {
                if let Ok(Some(aid)) = reference(&dest_url, "attachment:")
                    && !found.contains(&aid)
                {
                    found.push(aid);
                }
            }
            _ => {}
        }
    }
    found
}

/// The original values of all `<a href>` in the rendered body (in-site references not rewritten), used for blocking
/// external links from new members (`content::antispam`).
/// It goes through the same Markdown parsing and spoiler preprocessing as rendering; hand-written `<a>` is parsed by
/// the sanitizer, so everything clickable in Markdown links, autolinks and hand-written links is here. Code blocks are
/// text and never appear here.
pub fn link_targets(source: &str) -> Vec<String> {
    let source = spoilers(source);
    let mut output = String::new();
    html::push_html(&mut output, bounded_events(&source).into_iter());
    let found = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = std::sync::Arc::clone(&found);
    ammonia::Builder::default()
        .add_tags(&["details", "summary"])
        .attribute_filter(move |element, attribute, value| {
            if element == "a" && attribute == "href" {
                sink.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(value.to_owned());
            }
            Some(value.into())
        })
        .clean(&output);
    std::mem::take(
        &mut *found
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    )
}

pub async fn member_names(db: &Database, sources: &[&str]) -> Result<BTreeMap<i64, String>> {
    let mut ids = BTreeSet::new();
    for source in sources {
        ids.extend(references(source)?.members);
    }
    if ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let sql = &format!(
        "SELECT uid, username FROM gonof_common_member WHERE account_state<>'deleted' AND uid IN ({})",
        vec!["?"; ids.len()].join(",")
    );
    let mut query = crate::db::query(sql);
    for id in ids {
        query = query.bind(id);
    }
    let rows = query.fetch_all(db.executor()).await?;
    rows.into_iter()
        .map(|row| Ok((row.try_get("uid")?, row.try_get("username")?)))
        .collect()
}

/// The opening markup a `<spoiler>` turns into, in this request's language (the rendered-bodies cache key carries
/// it; docs/architecture.md, "Languages").
fn spoiler_open() -> String {
    format!(
        "<details class=\"reveal\"><summary>{}</summary>",
        escape(crate::i18n::current().markdown_spoiler())
    )
}

const SPOILER_CLOSE: &str = "</details>";

/// What a body brought in by `gono-forum-cli import-content` may not contain (docs/content-import.md, "Writing"):
/// images, which would need uploaded attachments; raw HTML other than the spoiler wrapper; and in-site references
/// (`attachment:`, `member:`, `post:`), which need real ids. Returns a short description of the first problem found.
pub(crate) fn imported_body_problem(source: &str) -> Option<&'static str> {
    let source = spoilers(source);
    let open = spoiler_open();
    for event in Parser::new_ext(&source, options()) {
        match event {
            Event::Start(Tag::Image { .. }) => return Some("an image"),
            Event::Start(Tag::Link { dest_url, .. })
                if ["attachment:", "member:", "post:"]
                    .iter()
                    .any(|prefix| dest_url.starts_with(prefix)) =>
            {
                return Some("an in-site reference link (attachment:, member: or post:)");
            }
            Event::Html(value) | Event::InlineHtml(value)
                if !value
                    .replace(&open, "")
                    .replace(SPOILER_CLOSE, "")
                    .trim()
                    .is_empty() =>
            {
                return Some("HTML other than <spoiler>");
            }
            _ => {}
        }
    }
    None
}

// Only recognize the explicit spoiler wrapper outside code. Arbitrary HTML remains
// untrusted and passes through the same allow-list cleaner as normal Markdown.
fn spoilers(source: &str) -> String {
    let summary = format!("\n\n{}\n\n", spoiler_open());
    let mut replacements = Vec::new();
    for (event, range) in Parser::new_ext(source, options()).into_offset_iter() {
        if let Event::Html(value) | Event::InlineHtml(value) = event
            && (value.contains("<spoiler>") || value.contains("</spoiler>"))
        {
            let text = value
                .replace("<spoiler>", &summary)
                .replace("</spoiler>", &format!("\n\n{SPOILER_CLOSE}\n\n"));
            replacements.push((range, text));
        }
    }
    // One pass over the source: replacing each range in place would move the rest of the text every time, which
    // is quadratic for a body made of thousands of spoilers. The ranges come in order and do not overlap.
    let mut output = String::with_capacity(source.len());
    let mut copied = 0;
    for (range, value) in replacements {
        if range.start < copied {
            continue;
        }
        output.push_str(&source[copied..range.start]);
        output.push_str(&value);
        copied = range.end;
    }
    output.push_str(&source[copied..]);
    output
}

/// Classes allowed to stay in the body, limited to the one the renderer itself produces: spoiler `details.reveal`.
/// All other class names are dropped, including the `language-…` class Markdown puts on a fenced code block's
/// `code`; otherwise users could write something like `<details class="write-fullscreen">` and borrow in-site
/// component styles to cover the whole page as a disguise.
///
/// Code blocks are not syntax highlighted. The highlighter's time depends on the grammar (one Makefile line could
/// take over 10 ms, a few kilobytes of CSS or shell half a second), and grammars embed one another, so no size or
/// language limit bounds it; bodies are rendered again on every signed-in page view.
fn rendered_class(element: &str, value: &str) -> Option<String> {
    let kept = value
        .split_ascii_whitespace()
        .filter(|class| element == "details" && *class == "reveal")
        .collect::<Vec<_>>();
    (!kept.is_empty()).then(|| kept.join(" "))
}

pub fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

pub fn render(source: &str, members: &BTreeMap<i64, String>, pid: Option<i64>) -> String {
    render_media(source, members, &|aid| media_path(aid, pid), Mode::Plain).html
}

/// Thread page body: a YouTube link standing as its own paragraph becomes a click-to-load placeholder, and an in-site
/// thread link standing as its own paragraph is marked as a card candidate, handed to the caller to fill in as a card
/// or plain link according to the reader's permissions ([`fill_cards`]).
/// `site_url` is this site's URL, used to recognise in-site links written as absolute URLs.
pub fn render_page(
    source: &str,
    members: &BTreeMap<i64, String>,
    pid: Option<i64>,
    site_url: &str,
) -> Rendered {
    render_media(
        source,
        members,
        &|aid| media_path(aid, pid),
        Mode::Page(site_url),
    )
}

/// Safe text fragment for compact summaries, preserving canonical member/quote labels.
pub fn summary(source: &str, members: &BTreeMap<i64, String>) -> String {
    plain_text(&render(source, members, None))
}

/// Plain text of a rendered body: all tags removed, spoilers (`details`) dropped together with their content, and
/// whitespace collapsed to a single space.
/// The result is still escaped HTML text.
fn plain_text(rendered: &str) -> String {
    ammonia::Builder::default()
        .tags(std::collections::HashSet::new())
        // Spoilers are dropped together with their content; YouTube placeholders (figure) and thread cards (aside) are added
        // by the renderer and are not this post's text, so they are dropped too.
        .add_clean_content_tags(&["details", "figure", "aside"])
        .clean(rendered)
        .to_string()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Plain-text summary for lists, at most `limit` characters.
///
/// Like `summary`, drops spoilers, shows mentions with the current username and does not reveal quote syntax, but
/// returns unescaped text left to the template's auto-escaping; so truncating by characters cannot cut a character
/// reference.
pub fn excerpt(source: &str, members: &BTreeMap<i64, String>, limit: usize) -> String {
    truncate(&summary(source, members), limit)
}

/// Takes the same plain-text summary from already rendered body HTML. Page descriptions use it: mentions were already
/// expanded with the current username at render time, so the member table need not be queried again for the summary.
pub fn rendered_excerpt(rendered: &str, limit: usize) -> String {
    truncate(&plain_text(rendered), limit)
}

fn truncate(summary: &str, limit: usize) -> String {
    unescape_text(summary).chars().take(limit).collect()
}

/// Restores character references in `summary` output. When the sanitizer serialises text nodes it only writes
/// `&amp;`, `&lt;`, `&gt;` and `&nbsp;`; other fragments starting with `&` are kept as is.
fn unescape_text(value: &str) -> String {
    const ENTITIES: [(&str, char); 4] = [
        ("&amp;", '&'),
        ("&lt;", '<'),
        ("&gt;", '>'),
        ("&nbsp;", '\u{a0}'),
    ];
    let mut text = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find('&') {
        text.push_str(&rest[..start]);
        rest = &rest[start..];
        match ENTITIES.iter().find(|(entity, _)| rest.starts_with(entity)) {
            Some((entity, character)) => {
                text.push(*character);
                rest = &rest[entity.len()..];
            }
            None => {
                text.push('&');
                rest = &rest[1..];
            }
        }
    }
    text.push_str(rest);
    text
}

/// Editor previews may mix existing post references and new private uploads.
/// Expands YouTube placeholders and thread card candidates as on the thread page.
pub fn render_preview(
    source: &str,
    members: &BTreeMap<i64, String>,
    paths: &BTreeMap<i64, String>,
    site_url: &str,
) -> Rendered {
    render_media(
        source,
        members,
        &|aid| {
            paths
                .get(&aid)
                .cloned()
                .unwrap_or_else(|| "#attachment-unavailable".into())
        },
        Mode::Page(site_url),
    )
}

/// The first-post excerpt in a thread card: drops spoilers, images, and the text of mentions, quotes and attachment
/// links (cards do not look up member names and do not reveal attachments), then truncates by characters. Returns
/// unescaped plain text, escaped by [`card_html`].
pub fn card_excerpt(source: &str, limit: usize) -> String {
    truncate(
        &plain_text(&render_media(source, &BTreeMap::new(), &media_placeholder, Mode::Card).html),
        limit,
    )
}

fn media_placeholder(_: i64) -> String {
    String::new()
}

#[derive(Clone, Copy)]
enum Mode<'a> {
    /// Plain rendering: all links are output as is. Used by summaries, feeds and policy pages.
    Plain,
    /// Body page: expands YouTube placeholders and marks thread card candidates.
    Page(&'a str),
    /// Card excerpt: mentions, quotes and attachment links are removed along with their text.
    Card,
}

/// Rendering result and card candidates. `html` is sanitized; before [`fill_cards`], card candidates are paragraphs
/// carrying a random marker. The anonymous render cache stores it as it is, so the cards are filled for each request
/// and show the linked threads as they are now.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Rendered {
    pub html: String,
    pub cards: Vec<CardSlot>,
}

/// One in-site thread card candidate: the marked paragraph in the body and the thread it points to.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CardSlot {
    marker: String,
    pub tid: i64,
}

/// Maximum number of YouTube placeholders and thread cards expanded per post; extras stay plain links.
pub const MAX_VIDEOS_PER_POST: usize = 5;
pub const MAX_CARDS_PER_POST: usize = 3;

/// A recognised YouTube video: the ID contains only `[A-Za-z0-9_-]` and is exactly 11 characters; the start time in
/// seconds is optional.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct YouTube {
    pub id: String,
    pub start: Option<u32>,
}

/// Only accepts YouTube URLs over https with an exact host match and no username or port.
pub fn youtube(value: &str) -> Option<YouTube> {
    let url = url::Url::parse(value).ok()?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        return None;
    }
    let host = url.host_str()?;
    let segments: Vec<&str> = url.path_segments()?.collect();
    let id = match (host, segments.as_slice()) {
        ("youtu.be", [id]) => (*id).to_owned(),
        ("youtube.com" | "www.youtube.com" | "m.youtube.com", ["watch"]) => url
            .query_pairs()
            .find(|(name, _)| name == "v")
            .map(|(_, value)| value.into_owned())?,
        (
            "youtube.com" | "www.youtube.com" | "m.youtube.com",
            ["shorts" | "live" | "embed", id],
        )
        | ("youtube-nocookie.com" | "www.youtube-nocookie.com", ["embed", id]) => (*id).to_owned(),
        _ => return None,
    };
    if id.len() != 11
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return None;
    }
    let start = url
        .query_pairs()
        .find(|(name, _)| name == "t" || name == "start")
        .and_then(|(_, value)| start_seconds(&value));
    Some(YouTube { id, start })
}

/// Start times in the forms `90`, `90s`, `1m30s`, `1h2m3s`, at most 100 hours; other forms are ignored.
fn start_seconds(value: &str) -> Option<u32> {
    if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) {
        return value.parse::<u32>().ok().filter(|v| *v > 0 && *v < 360_000);
    }
    let mut total: u32 = 0;
    let mut number = String::new();
    let mut last_unit = 0;
    for c in value.chars() {
        if c.is_ascii_digit() {
            number.push(c);
            continue;
        }
        let (unit, scale) = match c {
            'h' => (1, 3_600),
            'm' => (2, 60),
            's' => (3, 1),
            _ => return None,
        };
        if number.is_empty() || unit <= last_unit {
            return None;
        }
        total = total.checked_add(number.parse::<u32>().ok()?.checked_mul(scale)?)?;
        number.clear();
        last_unit = unit;
    }
    (number.is_empty() && total > 0 && total < 360_000).then_some(total)
}

/// The thread id an in-site link to a whole thread points to: `/thread-N`, or the same path starting with this site's
/// URL. A link with a page number (`/thread-N-2`), a query string (`?view=flat`, `?sub_root=…`) or an anchor
/// (`#floor-52`) points into the thread, and a card, which links to the thread's first page, would lose that; it gets
/// `None` and stays the link it was written as.
pub fn thread_link(value: &str, site_url: &str) -> Option<i64> {
    // Root-relative forms first strip this request's path prefix: both `/bbs/thread-5` and `/thread-5` are recognised.
    let unprefixed = crate::site::unprefixed(value);
    let path = if value.starts_with('/') {
        if value.starts_with("//") || value.starts_with("/\\") {
            return None;
        }
        unprefixed.as_ref()
    } else {
        // Besides this site's URL, when multi-domain sites are on, all enabled registered domains and the forum URL
        // also count as in-site. The longest address that matches decides: with a root site and a site under
        // `/community` on the same host, `…/community/thread-22` is the latter's thread, not the root site's path
        // `/community/thread-22`.
        let siblings = crate::site::sibling_urls();
        let mut sites: Vec<&str> = std::iter::once(site_url)
            .chain(siblings.iter().map(String::as_str))
            .map(|site| site.trim_end_matches('/'))
            .filter(|site| !site.is_empty())
            .collect();
        sites.sort_by_key(|site| std::cmp::Reverse(site.len()));
        sites.into_iter().find_map(|site| {
            value
                .strip_prefix(site)
                .filter(|rest| rest.starts_with('/'))
        })?
    };
    let id = path.strip_prefix("/thread-")?;
    if id.is_empty() || id.len() > 18 || !id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    id.parse::<i64>().ok().filter(|id| *id > 0)
}

enum Candidate {
    Video(YouTube, String),
    Topic(i64),
}

/// Finds top-level paragraphs (not in a blockquote, list or table) consisting only of one link or only of one address
/// text, and replaces the paragraph's opening and closing tags with `<p title="…">` carrying a random marker. After
/// sanitizing, the marker is still the element's start tag: it contains both `<` and `"`, and when the sanitizer
/// serialises user content, `<` in text and `"` in attribute values are both escaped, so users cannot write the same
/// string, and the random part cannot be guessed either; the replacement therefore only lands on the renderer's own
/// paragraphs.
fn mark_candidates<'a>(
    events: &mut [Event<'a>],
    site_url: &str,
    nonce: &str,
) -> Vec<(String, Candidate)> {
    let mut found = Vec::new();
    let (mut videos, mut cards) = (0, 0);
    let mut depth = 0_usize;
    let mut index = 0;
    while index < events.len() {
        match &events[index] {
            Event::Start(Tag::Paragraph) if depth == 0 => {
                let Some(end) = events[index..]
                    .iter()
                    .position(|e| matches!(e, Event::End(TagEnd::Paragraph)))
                    .map(|offset| index + offset)
                else {
                    break;
                };
                if let Some((url, label, linked)) = lone_link(&events[index + 1..end]) {
                    let candidate = if let Some(video) =
                        youtube(&url).filter(|_| videos < MAX_VIDEOS_PER_POST)
                    {
                        videos += 1;
                        let title = if label.is_empty() || label == url {
                            crate::i18n::current().markdown_video_title().to_owned()
                        } else {
                            label
                        };
                        Some(Candidate::Video(video, title))
                    } else if let Some(tid) = thread_link(&url, site_url)
                        // Bare text only counts as an absolute URL: `/thread-5` as text on its own line is not a link.
                        .filter(|_| cards < MAX_CARDS_PER_POST && (linked || !url.starts_with('/')))
                    {
                        cards += 1;
                        Some(Candidate::Topic(tid))
                    } else {
                        None
                    };
                    if let Some(candidate) = candidate {
                        let marker = format!("<p title=\"gonof-{nonce}-{}\">", found.len());
                        events[index] = Event::Html(marker.clone().into());
                        events[end] = Event::Html("</p>\n".into());
                        found.push((marker, candidate));
                    }
                }
                index = end + 1;
                continue;
            }
            Event::Start(_) => depth += 1,
            Event::End(_) => depth = depth.saturating_sub(1),
            _ => {}
        }
        index += 1;
    }
    found
}

/// When the paragraph content is only one link (whose text is only plain text and inline code), or only one run of
/// text without whitespace, returns the URL, the link text, and whether it was written as a link.
fn lone_link(inner: &[Event<'_>]) -> Option<(String, String, bool)> {
    match inner {
        [
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                ..
            }),
            middle @ ..,
            Event::End(TagEnd::Link),
        ] if !matches!(link_type, pulldown_cmark::LinkType::Email) => {
            let mut label = String::new();
            for event in middle {
                match event {
                    Event::Text(text) | Event::Code(text) => label.push_str(text),
                    _ => return None,
                }
            }
            Some((dest_url.to_string(), label.trim().to_owned(), true))
        }
        [] => None,
        texts => {
            let mut url = String::new();
            for event in texts {
                match event {
                    Event::Text(text) => url.push_str(text),
                    _ => return None,
                }
            }
            (!url.is_empty() && !url.chars().any(char::is_whitespace))
                .then(|| (url.clone(), url, false))
        }
    }
}

/// From the marker to this paragraph's `</p>`. The paragraph only contains inline content generated by the renderer,
/// so the first `</p>` is its end.
fn marked_range(html: &str, marker: &str) -> Option<std::ops::Range<usize>> {
    let start = html.find(marker)?;
    let end = html[start..].find("</p>")? + start + "</p>".len();
    Some(start..end)
}

/// YouTube placeholder: loads no external resources. With JavaScript, `site.js` shows a button and only inserts the
/// youtube-nocookie.com player after a click; without JavaScript there is only the external link.
fn video_html(video: &YouTube, title: &str) -> String {
    let start = video
        .start
        .map(|seconds| format!(" data-start=\"{seconds}\""))
        .unwrap_or_default();
    let watch = match video.start {
        Some(seconds) => format!(
            "https://www.youtube.com/watch?v={}&amp;t={seconds}s",
            video.id
        ),
        None => format!("https://www.youtube.com/watch?v={}", video.id),
    };
    // Interface text in this request's language (docs/architecture.md, "Languages").
    let lang = crate::i18n::current();
    format!(
        "<figure class=\"video-embed\" data-youtube=\"{id}\"{start}><figcaption class=\"video-embed__title\">{title}</figcaption><p class=\"video-embed__notice\">{notice}</p><p class=\"video-embed__actions\"><button type=\"button\" class=\"button button--primary video-embed__load\" data-media-load hidden>{load}</button><a class=\"video-embed__link\" href=\"{watch}\" rel=\"nofollow ugc noreferrer noopener\">{open}</a></p></figure>",
        id = video.id,
        title = escape(title),
        notice = escape(lang.markdown_video_notice()),
        load = escape(lang.markdown_video_load()),
        open = escape(lang.markdown_video_open()),
    )
}

/// The data of one thread card, looked up by the caller according to the reader's permissions.
#[derive(Clone, Debug)]
pub struct TopicCard {
    pub tid: i64,
    pub subject: String,
    pub forum_name: String,
    pub author: String,
    pub replies: i64,
    pub time: String,
    pub excerpt: String,
}

pub fn card_html(card: &TopicCard) -> String {
    let excerpt = if card.excerpt.is_empty() {
        String::new()
    } else {
        format!(
            "<p class=\"topic-embed__excerpt\">{}</p>",
            escape(&card.excerpt)
        )
    };
    format!(
        "<aside class=\"topic-embed\"><a class=\"topic-embed__title\" href=\"{prefix}/thread-{tid}\">{subject}</a><p class=\"topic-embed__meta\">{forum} · {author} · {replies} · {time}</p>{excerpt}</aside>",
        prefix = crate::site::prefix(),
        tid = card.tid,
        subject = escape(&card.subject),
        forum = escape(&card.forum_name),
        author = escape(&card.author),
        replies = escape(&crate::i18n::current().common_reply_count(card.replies)),
        time = escape(&card.time),
    )
}

/// Fills candidates with the cards the reader can see: paragraphs with a card are replaced by the card as a whole,
/// those without have the marker removed and show as the original plain link or text, with no title added and without
/// revealing whether the thread exists.
pub fn fill_cards(html: &str, slots: &[CardSlot], cards: &BTreeMap<i64, String>) -> String {
    let mut output = html.to_owned();
    for slot in slots {
        let Some(range) = marked_range(&output, &slot.marker) else {
            continue;
        };
        match cards.get(&slot.tid) {
            Some(card) => output.replace_range(range, card),
            None => output.replace_range(range.start..range.start + slot.marker.len(), "<p>"),
        }
    }
    output
}

fn render_media(
    source: &str,
    members: &BTreeMap<i64, String>,
    media: &dyn Fn(i64) -> String,
    mode: Mode<'_>,
) -> Rendered {
    let source = spoilers(source);
    let mut parsed = bounded_events(&source);
    let nonce = crate::identity::random_token();
    let candidates = match mode {
        Mode::Page(site_url) => mark_candidates(&mut parsed, site_url, &nonce[..32]),
        Mode::Plain | Mode::Card => Vec::new(),
    };
    let strip_references = matches!(mode, Mode::Card);
    let mut events = Vec::new();
    let mut fixed_label = false;
    let mut skip_link = 0_usize;
    let mut image_depth = 0;
    for event in parsed {
        if skip_link > 0 {
            match event {
                Event::Start(Tag::Link { .. }) => skip_link += 1,
                Event::End(TagEnd::Link) => skip_link -= 1,
                _ => {}
            }
            continue;
        }
        if fixed_label {
            if matches!(event, Event::End(TagEnd::Link)) {
                fixed_label = false;
                events.push(Event::End(TagEnd::Link));
            }
            continue;
        }
        match event {
            Event::Html(value) | Event::InlineHtml(value)
                if value.to_ascii_lowercase().contains("<img") =>
            {
                // Only Markdown attachment images may become live image elements.
            }
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                mut title,
                id,
            }) => {
                let mut destination = dest_url.to_string();
                let mut label = None;
                if strip_references
                    && ["member:", "post:", "attachment:"]
                        .iter()
                        .any(|prefix| dest_url.starts_with(prefix))
                {
                    skip_link = 1;
                    continue;
                }
                if image_depth == 0
                    && let Ok(Some(uid)) = reference(&dest_url, "member:")
                {
                    let name = members.get(&uid);
                    destination = if name.is_some() {
                        crate::urls::space(uid)
                    } else {
                        "#unknown-member".into()
                    };
                    label =
                        Some(name.map(|name| format!("@{name}")).unwrap_or_else(|| {
                            crate::i18n::current().common_deleted_user().into()
                        }));
                } else if image_depth == 0
                    && let Ok(Some(pid)) = reference(&dest_url, "post:")
                {
                    destination = format!("/posts/{pid}");
                    label = Some(crate::i18n::current().topic_quoted_post().into());
                } else if let Ok(Some(aid)) = reference(&dest_url, "attachment:") {
                    destination = media(aid);
                    // Site-wide `<body hx-boost="true">`: if an attachment link were boosted, htmx would swap the file
                    // content into body as a page and the address bar would become /media/…, instead of downloading.
                    // Links in image alt text only output text, and writing a start tag would be escaped as text, so
                    // only links outside images are handled.
                    if image_depth == 0 {
                        let title = if title.is_empty() {
                            String::new()
                        } else {
                            format!(" title=\"{}\"", escape(&title))
                        };
                        events.push(Event::InlineHtml(
                            format!(
                                "<a href=\"{}\"{title} hx-boost=\"false\">",
                                escape(&destination)
                            )
                            .into(),
                        ));
                        continue;
                    }
                }
                if label.is_some() {
                    title = "".into();
                    fixed_label = true;
                }
                events.push(Event::Start(Tag::Link {
                    link_type,
                    dest_url: destination.into(),
                    title,
                    id,
                }));
                if let Some(label) = label {
                    events.push(Event::Text(label.into()));
                }
            }
            Event::Start(Tag::Image {
                link_type,
                dest_url,
                title,
                id,
            }) => {
                image_depth += 1;
                let destination = match reference(&dest_url, "attachment:") {
                    Ok(Some(aid)) => media(aid),
                    _ => String::new(),
                };
                // Only managed images are embedded; arbitrary remote URLs remain inert.
                events.push(Event::Start(Tag::Image {
                    link_type,
                    dest_url: destination.into(),
                    title,
                    id,
                }));
            }
            Event::End(TagEnd::Image) => {
                image_depth -= 1;
                events.push(Event::End(TagEnd::Image));
            }
            event => events.push(event),
        }
    }
    let mut output = String::new();
    html::push_html(&mut output, events.into_iter());
    let mut html = ammonia::Builder::default()
        .add_tags(&["details", "summary"])
        .add_tag_attributes("details", &["class"])
        .add_tag_attributes("a", &["hx-boost"])
        // Body images load lazily: long threads need not fetch every original image on page load. Only
        // this one attribute is forced: the sanitizer outputs forced attributes in hash map order, so with more than
        // one their order could differ per process; nor can it be appended as a string after sanitizing, since `<`
        // in attribute values is not escaped.
        .set_tag_attribute_value("img", "loading", "lazy")
        .link_rel(Some("nofollow ugc noreferrer"))
        .attribute_filter(|element, attribute, value| {
            use std::borrow::Cow;
            match (element, attribute) {
                // In-site URLs get this request's path prefix, without adding it twice when already prefixed.
                // Images only allow this site's attachments, and prefixed forms pasted by members are recognised too.
                ("img", "src") => crate::site::unprefixed(value)
                    .starts_with("/media/")
                    .then(|| Cow::Owned(crate::site::content_href(value))),
                ("a", "href") => Some(Cow::Owned(crate::site::content_href(value))),
                // Can only be used to turn boost off; hand-written values are always changed to false.
                ("a", "hx-boost") => Some(Cow::Borrowed("false")),
                (_, "class") => rendered_class(element, value).map(Cow::Owned),
                _ => Some(Cow::Borrowed(value)),
            }
        })
        .clean(&output)
        .to_string();
    let mut cards = Vec::new();
    for (marker, candidate) in candidates {
        match candidate {
            Candidate::Video(video, title) => {
                if let Some(range) = marked_range(&html, &marker) {
                    html.replace_range(range, &video_html(&video, &title));
                }
            }
            Candidate::Topic(tid) => cards.push(CardSlot { marker, tid }),
        }
    }
    Rendered { html, cards }
}
fn media_path(aid: i64, pid: Option<i64>) -> String {
    pid.map(|pid| format!("/media/{aid}?pid={pid}"))
        .unwrap_or_else(|| format!("/media/{aid}"))
}

#[cfg(test)]
mod tests {

    #[test]
    fn imported_bodies_may_not_carry_images_html_or_site_references() {
        for clean in [
            "Plain **text** with a [link](https://example.com) and `code`.",
            "| a | b |\n| - | - |\n| 1 | 2 |",
            "Before\n\n<spoiler>The ending</spoiler>\n\nAfter",
            "Inline <spoiler>secret</spoiler> spoiler",
            "```html\n<b>code is fine</b>\n```",
            "<https://example.com/autolink>",
        ] {
            assert_eq!(imported_body_problem(clean), None, "{clean}");
        }
        for (body, problem) in [
            ("![cat](https://example.com/cat.png)", "an image"),
            ("<b>bold</b>", "HTML other than <spoiler>"),
            ("<!-- note -->", "HTML other than <spoiler>"),
            (
                "[file](attachment:3)",
                "an in-site reference link (attachment:, member: or post:)",
            ),
            (
                "[@nora](member:2)",
                "an in-site reference link (attachment:, member: or post:)",
            ),
            (
                "[quote](post:9)",
                "an in-site reference link (attachment:, member: or post:)",
            ),
        ] {
            assert_eq!(imported_body_problem(body), Some(problem), "{body}");
        }
    }

    use super::*;
    /// Task list checkboxes would be stripped by the sanitizer, brackets and all, so the original text is shown.
    #[test]
    fn task_list_markers_stay_literal_text() {
        let body = render("- [ ] to do\n- [x] done\n", &BTreeMap::new(), None);
        assert!(body.contains("[ ] to do"), "{body}");
        assert!(body.contains("[x] done"), "{body}");
        assert!(!body.contains("<input"), "{body}");
        // Hand-written input is still stripped.
        let raw = render("<input type=\"text\" value=\"x\">", &BTreeMap::new(), None);
        assert!(!raw.contains("<input"), "{raw}");
    }
    /// The publishing setting's attachment count cannot exceed the hard limit of body parsing.
    #[test]
    fn thumbnail_candidates_keep_image_order_and_skip_links_spoilers_and_code() {
        let source = "Start ![two](attachment:12) [file](attachment:30) ![one](attachment:5)\n\n<spoiler>\n\n![spoiler](attachment:40)\n\n</spoiler>\n\n`![code](attachment:50)`\n\n![again](attachment:12) ![external](https://example.test/a.png) ![three](attachment:7)";
        assert_eq!(image_attachments(source), vec![12, 5, 7]);
        assert!(image_attachments("[only a link](attachment:3)").is_empty());
    }

    #[test]
    fn attachment_limit_config_cannot_exceed_markdown_limit() {
        let mut config = crate::content::writes::WriteConfig::default();
        assert_eq!(config.max_attachments, MAX_ATTACHMENTS);
        config.validate().unwrap();
        config.max_attachments = MAX_ATTACHMENTS + 1;
        assert!(config.validate().is_err());
        config.max_attachments = 0;
        assert!(config.validate().is_err());
        let source: String = (1..=MAX_ATTACHMENTS as i64 + 1)
            .map(|aid| format!("[attachment](attachment:{aid}) "))
            .collect();
        assert!(references(&source).is_err());
    }
    #[test]
    fn summaries_omit_spoilers_without_exposing_collapsed_content() {
        let source = "Plain text\n\n<spoiler>\n\nThe hidden twist\n\n</spoiler>\n\nText after";
        let text = summary(source, &BTreeMap::new());
        assert!(text.contains("Plain text"));
        assert!(text.contains("Text after"));
        assert!(!text.contains("twist"), "{text}");
        assert!(summary("<spoiler>\n\nOnly a twist\n\n</spoiler>", &BTreeMap::new()).is_empty());
        let body = render(source, &BTreeMap::new(), None);
        assert!(body.contains("<details"));
        assert!(body.contains("The hidden twist"));
    }
    #[test]
    fn summaries_share_canonical_labels_and_never_embed_images_or_raw_html() {
        let text = summary(
            "[@stale name](member:4) [stale source](post:42 \"stale title\") ![picture](attachment:8) **body text** & <script>bad()</script> `literal <img>`",
            &BTreeMap::from([(4, "NewName".into())]),
        );
        assert!(text.contains("@NewName Quoted post"), "{text}");
        assert!(text.contains("body text"));
        assert!(text.contains("&amp;"));
        assert!(text.contains("&lt;img&gt;"));
        assert!(!text.contains("stale"), "{text}");
        assert!(!text.contains("bad()"));
        assert!(!text.contains('<'));
        assert!(!text.contains("attachment:"));
        assert!(!text.contains("post:"));
    }
    #[test]
    fn excerpts_are_plain_text_without_markup_spoilers_or_stale_names() {
        let members = BTreeMap::from([(4, "NewName".into())]);
        let text = excerpt(
            "[@stale name](member:4) says **a < b & c** ![picture](attachment:8) [download](attachment:9)\n\n<spoiler>\n\nspoiler text\n\n</spoiler>\n\n&amp;lt; `<b>`",
            &members,
            180,
        );
        assert_eq!(text, "@NewName says a < b & c download &lt; <b>");
        // Truncation is by character and never cuts a multi-byte character or a character reference. The escapes are
        // the Chinese numerals one to five, three bytes each in UTF-8.
        assert_eq!(
            excerpt("\u{4e00}\u{4e8c}\u{4e09} & \u{56db}\u{4e94}", &members, 5),
            "\u{4e00}\u{4e8c}\u{4e09} &"
        );
        assert_eq!(unescape_text("&amp;lt; &copy; &"), "&lt; &copy; &");
        // The page description taken from rendered HTML matches the one taken directly from Markdown.
        let source = "[@stale name](member:4) says **a < b**\n\n<spoiler>\n\nspoiler text\n\n</spoiler>\n\nThe end";
        assert_eq!(
            rendered_excerpt(&render(source, &members, Some(7)), 180),
            excerpt(source, &members, 180)
        );
        assert_eq!(
            rendered_excerpt("<p>\u{4e00}\u{4e8c}\u{4e09}\u{56db}\u{4e94}</p>", 3),
            "\u{4e00}\u{4e8c}\u{4e09}"
        );
    }
    #[test]
    fn references_are_syntax_aware() {
        let refs = references("![picture](attachment:12) [download](attachment:13) [@stale name](member:4) `![fake](attachment:99)`\n\n```\n[x](attachment:88)\n```").unwrap();
        assert_eq!(refs.attachments, BTreeSet::from([12, 13]));
        assert_eq!(refs.members, BTreeSet::from([4]));
        assert!(references("[bad](attachment:-1)").is_err());
    }
    #[test]
    fn notifications_only_use_explicit_links_and_limit_distinct_sources() {
        let refs = references("[@someone](member:4) [quote](post:12) [again](post:12) ![picture](member:5) ![picture](post:13) ![[@hidden](member:6) [hidden](post:14)](attachment:9)\n\n> plain quote\n\n`[code](post:15)`\n\n```\n[@code](member:7) [code](post:16)\n```").unwrap();
        assert_eq!(refs.members, BTreeSet::from([4]));
        assert_eq!(refs.quotes, BTreeSet::from([12]));
        assert_eq!(refs.attachments, BTreeSet::from([9]));
        for invalid in ["post:-1", "post:0", "post:1/x", "post:9223372036854775808"] {
            assert!(references(&format!("[quote]({invalid})")).is_err());
        }
        let fifty = (1..=50)
            .map(|id| format!("[quote](post:{id})"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(references(&fifty).unwrap().quotes.len(), 50);
        assert!(references(&format!("{fifty} [quote](post:51)")).is_err());
    }
    #[test]
    fn quote_links_never_render_untrusted_source_snapshots() {
        let html = render(
            "[stale body <b>stale author</b> ![stale picture](attachment:8)](post:42 \"stale title\")",
            &BTreeMap::new(),
            None,
        );
        assert!(html.contains("href=\"/posts/42\""));
        assert!(html.contains(">Quoted post</a>"), "{html}");
        assert!(!html.contains("stale"), "{html}");
        assert!(!html.contains("<img"));
        assert!(!html.contains("title="));
        assert!(!html.contains("post:"));
    }
    #[test]
    fn shared_renderer_sanitizes_and_resolves() {
        let html = render(
            "[@stale](member:4) ![picture](attachment:12)\n\n<script>alert(1)</script>\n\n[x](javascript:alert)\n\n<spoiler>\n\n**hidden**\n\n</spoiler>",
            &BTreeMap::from([(4, "NewName".into())]),
            Some(33),
        );
        assert!(html.contains("@NewName"));
        assert!(html.contains("/media/12?pid=33"));
        assert!(html.contains("<details"));
        assert!(html.contains("<strong>hidden</strong>"));
        assert!(!html.contains("<script"));
        assert!(!html.contains("javascript:"));
    }
    #[test]
    fn multiline_spoilers_share_reference_parsing_and_raw_images_stay_inert() {
        let source = "<spoiler>\n![picture](attachment:12) [@stale](member:4)\n</spoiler>";
        let refs = references(source).unwrap();
        assert_eq!(refs.attachments, BTreeSet::from([12]));
        assert_eq!(refs.members, BTreeSet::from([4]));
        let rendered = render(source, &BTreeMap::from([(4, "NewName".into())]), Some(33));
        assert!(rendered.contains("/media/12?pid=33"));
        assert!(rendered.contains("@NewName"));
        let raw = render("<IMG src=\"/media/12?pid=33\">", &BTreeMap::new(), Some(34));
        assert!(!raw.contains("<img"));
        assert!(!raw.contains("src="));
    }
    #[test]
    fn attachment_links_opt_out_of_boost() {
        let html = render(
            "[file a.pdf](attachment:9 \"description\") [![preview](attachment:8)](attachment:8) ![[inner](attachment:6)](attachment:7) [@stale](member:4) [quote](post:5) [in site](/rules) <a href=\"/x\" hx-boost=\"true\">hand-written</a>",
            &BTreeMap::from([(4, "NewName".into())]),
            Some(3),
        );
        assert!(html.contains(
            "<a href=\"/media/9?pid=3\" title=\"description\" hx-boost=\"false\" rel=\"nofollow ugc noreferrer\">file a.pdf</a>"
        ));
        assert!(html.contains("<a href=\"/media/8?pid=3\" hx-boost=\"false\" rel=\"nofollow ugc noreferrer\"><img src=\"/media/8?pid=3\" alt=\"preview\" loading=\"lazy\"></a>"));
        // Links in image alt text keep only their text and are not escaped into visible tags.
        assert!(html.contains("<img src=\"/media/7?pid=3\" alt=\"inner\" loading=\"lazy\">"));
        assert!(!html.contains("&lt;a"));
        // Other in-site links are boosted as usual; a hand-written hx-boost can only turn boost off.
        assert!(html.contains("<a href=\"/space/4\" rel=\"nofollow ugc noreferrer\">@NewName</a>"));
        assert!(
            html.contains("<a href=\"/posts/5\" rel=\"nofollow ugc noreferrer\">Quoted post</a>")
        );
        assert!(html.contains("<a href=\"/rules\" rel=\"nofollow ugc noreferrer\">in site</a>"));
        assert!(html.contains("<a href=\"/x\" hx-boost=\"false\""));
        assert!(!html.contains("hx-boost=\"true\""));
        // The editor preview handles it the same way.
        let preview = render_preview(
            "[file](attachment:9)",
            &BTreeMap::new(),
            &BTreeMap::from([(9, "/media/9".into())]),
            "http://127.0.0.1:58833",
        )
        .html;
        assert!(preview.contains("<a href=\"/media/9\" hx-boost=\"false\""));
    }
    #[test]
    fn classes_are_limited_to_what_the_renderer_emits() {
        let html = render(
            "<details class=\"write-fullscreen reveal\" open><summary>System notice</summary>\n\n<span class=\"banner syn-keyword\">disguised</span> <span class=\"button--primary\">button</span>\n\n</details>\n\n<pre class=\"write-fullscreen code-block\">fake code</pre>\n\n<p class=\"skip-link\">paragraph</p>\n\n<spoiler>\n\nspoiler\n\n</spoiler>\n\n```rust\nlet x = 1; // <comment>\n```",
            &BTreeMap::new(),
            None,
        );
        for forbidden in [
            "write-fullscreen",
            "banner",
            "button--primary",
            "skip-link",
            "syn-",
            "code-block",
            "language-",
        ] {
            assert!(!html.contains(forbidden), "{forbidden}: {html}");
        }
        assert!(html.contains("<details class=\"reveal\"><summary>System notice</summary>"));
        assert!(html.contains("<span>disguised</span> <span>button</span>"));
        assert!(html.contains("<pre>fake code</pre>"));
        assert!(html.contains("<details class=\"reveal\"><summary>Show spoiler</summary>"));
        // Code blocks are not highlighted: the code is escaped text in a plain `pre > code`.
        assert!(
            html.contains("<pre><code>let x = 1; // &lt;comment&gt;\n</code></pre>"),
            "{html}"
        );
    }
    const SITE: &str = "https://forum.example";

    fn page(source: &str) -> Rendered {
        render_page(source, &BTreeMap::new(), Some(7), SITE)
    }

    #[test]
    fn youtube_links_need_exact_https_hosts_and_valid_ids() {
        for (url, id, start) in [
            (
                "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
                "dQw4w9WgXcQ",
                None,
            ),
            (
                "https://youtube.com/watch?feature=x&v=dQw4w9WgXcQ&t=1m30s",
                "dQw4w9WgXcQ",
                Some(90),
            ),
            (
                "https://m.youtube.com/watch?v=a-b_c-d_e-f",
                "a-b_c-d_e-f",
                None,
            ),
            ("https://youtu.be/dQw4w9WgXcQ?t=42", "dQw4w9WgXcQ", Some(42)),
            (
                "https://www.youtube.com/shorts/dQw4w9WgXcQ",
                "dQw4w9WgXcQ",
                None,
            ),
            (
                "https://www.youtube.com/live/dQw4w9WgXcQ?start=3600",
                "dQw4w9WgXcQ",
                Some(3600),
            ),
            (
                "https://www.youtube.com/embed/dQw4w9WgXcQ",
                "dQw4w9WgXcQ",
                None,
            ),
            (
                "https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ?t=1h",
                "dQw4w9WgXcQ",
                Some(3600),
            ),
            (
                "HTTPS://WWW.YOUTUBE.COM/watch?v=dQw4w9WgXcQ&t=bad",
                "dQw4w9WgXcQ",
                None,
            ),
        ] {
            assert_eq!(
                youtube(url),
                Some(YouTube {
                    id: id.into(),
                    start
                }),
                "{url}"
            );
        }
        for url in [
            "http://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://www.youtube.com.evil.example/watch?v=dQw4w9WgXcQ",
            "https://evil.example/www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://evil.example/?u=https://youtu.be/dQw4w9WgXcQ",
            "https://user@www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://www.youtube.com:8443/watch?v=dQw4w9WgXcQ",
            "https://www.youtube.com/watch?v=dQw4w9WgXc",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQQ",
            "https://www.youtube.com/watch?v=dQw4w9WgX%22Q",
            "https://www.youtube.com/watch?v=dQw4w9WgX\"Q",
            "https://www.youtube.com/watch?v=<script>xx",
            "https://youtu.be/dQw4w9WgXcQ/extra",
            "https://www.youtube.com/channel/dQw4w9WgXcQ",
            "https://youtube-nocookie.com/watch?v=dQw4w9WgXcQ",
            "https://music.youtube.com/watch?v=dQw4w9WgXcQ",
            "javascript:alert('https://youtu.be/dQw4w9WgXcQ')",
            "data:text/html,https://youtu.be/dQw4w9WgXcQ",
            "//www.youtube.com/watch?v=dQw4w9WgXcQ",
        ] {
            assert_eq!(youtube(url), None, "{url}");
        }
    }

    #[test]
    fn only_standalone_youtube_paragraphs_become_click_to_load_placeholders() {
        let html = page(
            "https://youtu.be/dQw4w9WgXcQ?t=90\n\n[My <1> video](https://www.youtube.com/watch?v=a-b_c-d_e-f)\n\nIn a sentence [a link](https://youtu.be/zzzzzzzzzzz) stays a link\n\n> https://youtu.be/qqqqqqqqqqq\n\n- https://youtu.be/lllllllllll\n\n<spoiler>\n\n<https://youtu.be/sssssssssss>\n\n</spoiler>\n\n[malicious](https://www.youtube.com.evil.example/watch?v=dQw4w9WgXcQ)\n\n[x](javascript:alert(1))",
        )
        .html;
        assert_eq!(
            html.matches("<figure class=\"video-embed\"").count(),
            3,
            "{html}"
        );
        assert!(html.contains("data-youtube=\"dQw4w9WgXcQ\" data-start=\"90\""));
        assert!(
            html.contains("<figcaption class=\"video-embed__title\">YouTube video</figcaption>")
        );
        // `<1>` is not an HTML tag, so it stays in the link text and is escaped in the caption.
        assert!(
            html.contains(
                "<figcaption class=\"video-embed__title\">My &lt;1&gt; video</figcaption>"
            )
        );
        assert!(html.contains("href=\"https://www.youtube.com/watch?v=dQw4w9WgXcQ&amp;t=90s\""));
        // A standalone paragraph inside a spoiler is expanded too, and the placeholder stays inside the spoiler.
        let spoiler = &html[html.find("<details").unwrap()..html.find("</details>").unwrap()];
        assert!(spoiler.contains("data-youtube=\"sssssssssss\""));
        // Links within a sentence, in a blockquote or a list, and invalid URLs, are all plain links or text.
        for id in ["zzzzzzzzzzz", "qqqqqqqqqqq", "lllllllllll"] {
            assert!(!html.contains(&format!("data-youtube=\"{id}\"")), "{id}");
            assert!(html.contains(id), "{id}");
        }
        assert!(html.contains("href=\"https://www.youtube.com.evil.example/watch?v=dQw4w9WgXcQ\""));
        assert!(!html.contains("javascript:"));
        // Nothing loads before the click: no iframe, no thumbnail, and the player URL is only assembled in site.js.
        for forbidden in ["<iframe", "ytimg", "youtube-nocookie.com/embed", "<img"] {
            assert!(!html.contains(forbidden), "{forbidden}: {html}");
        }
        assert!(
            html.contains("youtube-nocookie.com, and YouTube (Google) receives your IP address")
        );
        // At most 5 placeholders per post; the rest stay as they are.
        let many: String = (0..7)
            .map(|n| format!("https://youtu.be/aaaaaaaaaa{n}\n\n"))
            .collect();
        let html = page(&many).html;
        assert_eq!(html.matches("data-youtube=").count(), MAX_VIDEOS_PER_POST);
        assert!(html.contains("<p>https://youtu.be/aaaaaaaaaa6</p>"));
        // Summaries, page descriptions and plain rendering do not expand placeholders, nor carry hint text.
        let source = "Before\n\nhttps://youtu.be/dQw4w9WgXcQ\n\nAfter";
        assert!(!render(source, &BTreeMap::new(), None).contains("video-embed"));
        assert_eq!(
            summary(source, &BTreeMap::new()),
            "Before https://youtu.be/dQw4w9WgXcQ After"
        );
        assert_eq!(rendered_excerpt(&page(source).html, 200), "Before After");
    }

    #[test]
    fn handwritten_frames_and_forged_placeholders_are_stripped() {
        let html = page(
            "<iframe src=\"https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ\"></iframe>\n\n<figure class=\"video-embed\" data-youtube=\"dQw4w9WgXcQ\"><button type=\"button\" data-media-load>Load player</button></figure>\n\n<p title=\"gonof-00000000000000000000000000000000-0\">guessed marker</p>\n\n<aside class=\"topic-embed\"><a class=\"topic-embed__title\" href=\"/thread-1\">fake card</a></aside>\n\n<a href=\"javascript:alert(1)\" onclick=\"x()\">tap</a>",
        );
        let html = html.html;
        for forbidden in [
            "<iframe",
            "<button",
            "data-youtube",
            "data-media-load",
            "class=\"video-embed\"",
            "class=\"topic-embed",
            "javascript:",
            "onclick",
        ] {
            assert!(!html.contains(forbidden), "{forbidden}: {html}");
        }
        // A guessed marker is just a normal paragraph and is never replaced by a placeholder or card.
        assert!(html.contains("guessed marker"));
        assert!(fill_cards(&html, &[], &BTreeMap::new()).contains("guessed marker"));
    }

    /// If a marked paragraph were swallowed into an attribute value by a user's unclosed attribute, its quotes would be
    /// escaped on serialisation and the string would not match, so no replacement happens and nothing inside the
    /// attribute value is released; markers that were not swallowed are still real elements, and the substituted card
    /// lands outside the attribute.
    #[test]
    fn markers_swallowed_by_unclosed_attributes_are_left_alone() {
        let source = "<div title=\"\n\nhttps://youtu.be/dQw4w9WgXcQ\n\n[card](/thread-5)\n\n\"><img src=x onerror=alert(1)>";
        let rendered = page(source);
        let filled = fill_cards(
            &rendered.html,
            &rendered.cards,
            &BTreeMap::from([(5, "<aside class=\"topic-embed\">card</aside>".to_owned())]),
        );
        for forbidden in ["<figure", "onerror", "<img"] {
            assert!(!filled.contains(forbidden), "{forbidden}: {filled}");
        }
        let start = filled.find("<div title=\"").expect(&filled);
        let tag = &filled[start..start + filled[start..].find('>').unwrap()];
        assert!(
            !tag.contains("aside") && !tag.contains("figure"),
            "{filled}"
        );
        assert!(!filled.contains("title=\"gonof-"), "{filled}");
    }

    #[test]
    fn standalone_site_thread_links_become_card_slots() {
        let rendered = page(
            "[look at this](/thread-5)\n\nhttps://forum.example/thread-6\n\n<https://forum.example/thread-7>\n\nIn a sentence [a link](/thread-8)\n\n> [in a quote](/thread-9)\n\n[bad](/thread-0)\n\n[bad](/thread-5x)\n\n[bad](//evil.example/thread-5)\n\n[bad](https://evil.example/thread-5)\n\n[bad](https://forum.example.evil/thread-5)\n\n[page number](/thread-5-01)",
        );
        assert_eq!(
            rendered
                .cards
                .iter()
                .map(|slot| slot.tid)
                .collect::<Vec<_>>(),
            [5, 6, 7]
        );
        // A page, view or anchor stays a link (`links_into_a_thread_stay_links`).
        assert!(
            page("https://forum.example/thread-6-2?view=flat#floor-3")
                .cards
                .is_empty()
        );
        // Relative-path text on its own line is not a link and gets no card; text with an absolute URL is still
        // recognised.
        assert!(page("/thread-5").cards.is_empty());
        // From the 4th on, the per-post limit is exceeded, so they stay plain links.
        let many = page("[a](/thread-1)\n\n[b](/thread-2)\n\n[c](/thread-3)\n\n[d](/thread-4)");
        assert_eq!(many.cards.len(), MAX_CARDS_PER_POST);
        // Threads that cannot be read show as the same link as plain rendering; readable ones replace the whole
        // paragraph with a card.
        let filled = fill_cards(
            &rendered.html,
            &rendered.cards,
            &BTreeMap::from([(
                6,
                card_html(&TopicCard {
                    tid: 6,
                    subject: "Title <b>".into(),
                    forum_name: "Board".into(),
                    author: "Author".into(),
                    replies: 3,
                    time: "2026-10-31 10:00".into(),
                    excerpt: "Excerpt & more".into(),
                }),
            )]),
        );
        assert!(filled.contains(
            "<p><a href=\"/thread-5\" rel=\"nofollow ugc noreferrer\">look at this</a></p>"
        ));
        assert!(filled.contains("<aside class=\"topic-embed\"><a class=\"topic-embed__title\" href=\"/thread-6\">Title &lt;b&gt;</a><p class=\"topic-embed__meta\">Board · Author · 3 replies · 2026-10-31 10:00</p><p class=\"topic-embed__excerpt\">Excerpt &amp; more</p></aside>"));
        assert!(filled.contains("<p><a href=\"https://forum.example/thread-7\" rel=\"nofollow ugc noreferrer\">https://forum.example/thread-7</a></p>"));
        assert!(!filled.contains("gonof-"), "{filled}");
        let plain = render("[look at this](/thread-5)", &BTreeMap::new(), Some(7));
        assert!(filled.contains(plain.trim()), "{plain}");
    }

    #[test]
    fn card_excerpts_drop_spoilers_attachments_mentions_and_quotes() {
        let text = card_excerpt(
            "Start [@someone](member:4) says ![picture](attachment:8) [secret.pdf](attachment:9) [quote](post:3) [external](https://example.com)\n\n<spoiler>\n\nspoiler secret\n\n</spoiler>\n\nThe end",
            100,
        );
        assert_eq!(text, "Start says external The end");
        // The limit counts characters: "\u{5b57}" (the Chinese for "character") takes three bytes in UTF-8.
        assert_eq!(
            card_excerpt(&"\u{5b57}".repeat(150), 100).chars().count(),
            100
        );
    }

    /// The deepest nesting of elements in rendered HTML. Void elements do not nest; text never holds a raw `<`.
    fn nesting(html: &str) -> usize {
        const VOID: [&str; 4] = ["br", "hr", "img", "wbr"];
        let (mut depth, mut deepest) = (0_usize, 0_usize);
        for piece in html.split('<').skip(1) {
            let (closing, rest) = match piece.strip_prefix('/') {
                Some(rest) => (true, rest),
                None => (false, piece),
            };
            let name: String = rest
                .chars()
                .take_while(char::is_ascii_alphanumeric)
                .collect();
            if name.is_empty() || VOID.contains(&name.as_str()) {
                continue;
            }
            if closing {
                depth = depth.saturating_sub(1);
            } else {
                depth += 1;
                deepest = deepest.max(depth);
            }
        }
        deepest
    }

    #[test]
    fn markdown_nesting_is_flattened_past_the_limit() {
        let members = BTreeMap::new();
        let html = render(&format!("{} inner", ">".repeat(10)), &members, None);
        assert_eq!(html.matches("<blockquote>").count(), 10);
        assert!(html.contains("<p>inner</p>"), "{html}");
        // Deeper levels are dropped; their text stays inside the deepest quote that is kept.
        let html = render(&format!("{} inner", ">".repeat(100)), &members, None);
        assert_eq!(html.matches("<blockquote>").count(), MAX_NESTING);
        assert_eq!(nesting(&html), MAX_NESTING);
        assert!(html.contains("inner"), "{html}");
        let html = render(&format!("{}item", "1. ".repeat(100)), &members, None);
        assert_eq!(nesting(&html), MAX_NESTING);
        assert!(html.contains("item"), "{html}");
        // Emphasis and links inside a deep quote keep their text, and a link past the limit is plain text.
        let html = render(
            &format!(
                "{} **bold** [link](https://example.com)",
                "> ".repeat(MAX_NESTING)
            ),
            &members,
            None,
        );
        assert!(
            html.contains("bold") && !html.contains("<strong>"),
            "{html}"
        );
        assert!(html.contains("link") && !html.contains("href"), "{html}");
        assert_eq!(
            link_targets(&format!("{}<a href=\"/x\">x</a>", "> ".repeat(100))),
            ["/x"]
        );
    }

    #[test]
    fn raw_html_past_the_tag_budget_is_shown_as_text() {
        let members = BTreeMap::new();
        let kept = "<b>x</b> ".repeat(MAX_HTML_TAGS);
        assert_eq!(
            render(&kept, &members, None).matches("<b>").count(),
            MAX_HTML_TAGS
        );
        // From the tag that goes past the budget on, raw HTML is text, end tags included.
        let html = render(&format!("{kept}<b>y</b> <i>z</i>"), &members, None);
        assert_eq!(html.matches("<b>").count(), MAX_HTML_TAGS);
        assert!(
            html.contains("&lt;b&gt;y&lt;/b&gt; &lt;i&gt;z&lt;/i&gt;"),
            "{html}"
        );
        // A spoiler uses two start tags; one past the budget shows the tags the member typed.
        let spoilers: String = (0..=MAX_HTML_TAGS / 2)
            .map(|n| format!("<spoiler>\n\nsecret {n}\n\n</spoiler>\n\n"))
            .collect();
        let html = render(&spoilers, &members, None);
        assert_eq!(
            html.matches("<details class=\"reveal\">").count(),
            MAX_HTML_TAGS / 2
        );
        assert!(
            html.contains("&lt;spoiler&gt;") && html.contains("&lt;/spoiler&gt;"),
            "{html}"
        );
        assert!(!html.contains("&lt;details"), "{html}");
        // The anti-spam check sees the links the page shows, and no more.
        let links: String = (0..MAX_HTML_TAGS + 10)
            .map(|n| format!("<a href=\"https://example.com/{n}\">{n}</a> "))
            .collect();
        assert_eq!(link_targets(&links).len(), MAX_HTML_TAGS);
        assert_eq!(
            render(&links, &members, None).matches("<a ").count(),
            MAX_HTML_TAGS
        );
    }

    /// Bodies at the size limit that nest as deeply as they can. Without the limits the sanitizer spends from
    /// a fraction of a second to more than half a minute on each of them in a release build; with them the
    /// output stays shallow.
    #[test]
    fn nesting_floods_render_shallow() {
        let limit = crate::content::writes::MAX_BODY_BYTES;
        let members = BTreeMap::new();
        for flood in [
            ">".repeat(limit),
            "> ".repeat(limit / 2),
            "1. ".repeat(limit / 3),
            "> - ".repeat(limit / 4),
            "* ".repeat(limit / 2),
            "**a ".repeat(limit / 4),
            "[a".repeat(limit / 2),
        ] {
            for html in [
                render(&flood, &members, None),
                page(&flood).html,
                card_excerpt(&flood, 100),
            ] {
                assert!(nesting(&html) <= MAX_NESTING, "{}", &flood[..12]);
            }
        }
        for flood in [
            "<div>".repeat(limit / 5),
            "<div>\n".repeat(limit / 6),
            "<ul><li>".repeat(limit / 8),
            "<spoiler>".repeat(limit / 9),
            "<spoiler>\n\nx\n\n".repeat(limit / 14),
        ] {
            let html = render(&flood, &members, None);
            assert!(nesting(&html) <= MAX_HTML_TAGS, "{}", &flood[..12]);
        }
    }

    /// Building the output in one pass gives the same text as replacing each range in place, back to front.
    #[test]
    fn spoilers_match_replacing_each_range_in_place() {
        let summary = format!("\n\n{}\n\n", spoiler_open());
        for source in [
            "a <spoiler>b</spoiler> c",
            "<spoiler>\n\nd\n\n</spoiler>\n\nafter",
            "> <spoiler>\n> quoted\n> </spoiler>\n\n- <spoiler>item</spoiler>\n- <b><spoiler>x</spoiler></b>",
            "`<spoiler>` and\n\n```\n<spoiler>\n```\n\n<div><spoiler>raw</spoiler></div>",
            "<spoiler><spoiler><spoiler>deep</spoiler></spoiler></spoiler>",
            "no spoilers here",
        ] {
            let mut expected = source.to_owned();
            let ranges: Vec<_> = Parser::new_ext(source, options())
                .into_offset_iter()
                .filter_map(|(event, range)| match event {
                    Event::Html(value) | Event::InlineHtml(value)
                        if value.contains("<spoiler>") || value.contains("</spoiler>") =>
                    {
                        Some((
                            range,
                            value
                                .replace("<spoiler>", &summary)
                                .replace("</spoiler>", &format!("\n\n{SPOILER_CLOSE}\n\n")),
                        ))
                    }
                    _ => None,
                })
                .collect();
            for (range, value) in ranges.into_iter().rev() {
                expected.replace_range(range, &value);
            }
            assert_eq!(spoilers(source), expected, "{source}");
        }
    }

    #[test]
    fn markdown_retains_multiline_blocks() {
        let html = render(
            "|A|B|\n|-|-|\n|1|2|\n\n```rust\nlet x = 1;\n\nlet y = 2;\n```",
            &BTreeMap::new(),
            None,
        );
        assert!(html.contains("<table>"));
        assert!(html.contains("<pre"));
        assert!(html.contains("let"));
    }

    /// A link to a page, a reading view or one reply of a thread keeps that destination: a card would only lead to
    /// the thread's first page, so such links stay the links they were written as.
    #[test]
    fn links_into_a_thread_stay_links() {
        for (source, shown) in [
            (
                "https://forum.example/thread-103-2?view=flat#floor-52",
                "<p>https://forum.example/thread-103-2?view=flat#floor-52</p>",
            ),
            ("[second page](/thread-5-2)", "href=\"/thread-5-2\""),
            (
                "[flat view](/thread-5?view=flat)",
                "href=\"/thread-5?view=flat\"",
            ),
            (
                "[one reply](/thread-5#floor-52)",
                "href=\"/thread-5#floor-52\"",
            ),
            (
                "[its replies](/thread-5?sub_root=9&sub_page=2)",
                "href=\"/thread-5?sub_root=9&amp;sub_page=2\"",
            ),
            (
                "<https://forum.example/thread-5#floor-3>",
                "href=\"https://forum.example/thread-5#floor-3\"",
            ),
        ] {
            let rendered = page(source);
            assert!(rendered.cards.is_empty(), "{source}");
            let filled = fill_cards(&rendered.html, &rendered.cards, &BTreeMap::new());
            assert!(filled.contains(shown), "{filled}");
            assert!(!filled.contains("gonof-"), "{filled}");
        }
        // The thread itself still becomes a card, written relative or absolute.
        assert_eq!(page("[the thread](/thread-5)").cards.len(), 1);
        assert_eq!(page("https://forum.example/thread-5").cards.len(), 1);
    }

    /// With a root site and a site under `/community` on the same host, an absolute link under `/community` belongs
    /// to the longer address, whichever order the sites come in.
    #[test]
    fn the_longest_registered_site_address_recognizes_a_thread_link() {
        use crate::domains::{DomainMap, tests::row};
        use crate::site::{Domains, Site, sync_scope};
        for rows in [
            vec![
                row(1, "https://forum.example", &[]),
                row(2, "https://forum.example/community", &[]),
            ],
            vec![
                row(1, "https://forum.example/community", &[]),
                row(2, "https://forum.example", &[]),
            ],
        ] {
            let forum_url = rows[0].site_url.clone();
            let site = Site {
                domains: Some(std::sync::Arc::new(Domains {
                    map: std::sync::Arc::new(DomainMap::new(rows)),
                    forum_url: forum_url.into(),
                    current: Some(1),
                })),
                ..Site::default()
            };
            sync_scope(site, || {
                for site_url in ["https://forum.example", "https://forum.example/community"] {
                    for (link, tid) in [
                        ("https://forum.example/community/thread-22", Some(22)),
                        ("https://forum.example/thread-23", Some(23)),
                        ("https://forum.example/community/rules", None),
                        ("https://forum.example/communityx/thread-24", None),
                    ] {
                        assert_eq!(thread_link(link, site_url), tid, "{link} on {site_url}");
                    }
                }
            });
        }
    }
}
