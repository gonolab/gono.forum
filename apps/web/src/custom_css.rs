//! Member custom styles (the `custom_css` and `custom_css_version` columns of `gonof_home_reading_preference`).
//!
//! - **Only applies when the member browses themselves**: the stylesheet URL only appears on the member's
//!   own pages; others, visitors included, never get it when viewing the member's profile. Edit pages, the admin
//!   area, account security, sign-in and two-factor authentication, sensitive pages and error pages do not apply
//!   it, so a member who breaks it can always see the original and change it back.
//! - **Strict validation on save, rejecting the whole thing**; the database stores the submitted text:
//!   limit 16 KiB; any construct that can issue a request is rejected (`url()`, `image-set()`, `@import`,
//!   `@font-face`, every @ rule other than `@media`, site URLs too); disguise protection: no
//!   `position: fixed`, `content` can only be `none`, `normal` or an empty string, `z-index` only `auto` or -1 to
//!   20 (the page header is 30); backslash escapes are always rejected (against bypasses like `u\72l(`); functions
//!   in values go through an allowlist; `--custom-properties` and `!important` are allowed; brackets and
//!   functions nest at most 16 levels deep inside one `@media` condition, selector or value.
//! - **Delivery**: a same-site external stylesheet `/my/style.css?u={uid}&v={version}`, already allowed by
//!   the CSP's `style-src 'self'`, so the CSP does not change; the stylesheet is not inlined into the HTML, so
//!   there is no `</style>` truncation.
//!
//! The current CSP's `img-src 'self' data:` and `default-src 'self'` block off-site images and fonts; that is
//! only a second line of defense.
use std::borrow::Cow;

use cssparser::{ParseError, ParseErrorKind, Parser, ParserInput, Token};

use crate::{
    content::writes::ContentWriteError,
    db::{self, Database},
    identity::Actor,
    interaction,
};

/// Byte limit of a custom style (UTF-8).
pub const MAX_BYTES: usize = 16 * 1024;
/// Rule count limit; rules inside `@media` count too.
const MAX_RULES: usize = 1000;
/// How deeply brackets and functions may nest inside one `@media` condition, selector or value. Real styles stay
/// within a few levels (`color-mix(in oklch, var(--a, rgb(0 0 0 / calc(…))), …)` is four). The validator
/// recurses once per level and runs on the request's worker thread, so without this limit a style of nothing but
/// `(` (about 16,000 levels within `MAX_BYTES`) overflows the stack and aborts the whole process.
const MAX_NESTING: usize = 16;

/// Why it does not comply, and where. `line` starts at 1; `None` for problems not tied to a position (too long).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    pub line: Option<u32>,
    pub reason: Cow<'static, str>,
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.line {
            Some(line) => f.write_str(
                &crate::i18n::current().settings_css_line(line.to_string(), &*self.reason),
            ),
            None => f.write_str(&self.reason),
        }
    }
}

type Failure<'i> = ParseError<'i, Cow<'static, str>>;

fn reject<'i>(parser: &Parser<'i, '_>, reason: impl Into<Cow<'static, str>>) -> Failure<'i> {
    parser.new_custom_error(reason.into())
}

/// The depth inside the block the parser is about to enter, or a rejection past `MAX_NESTING`. Called before
/// every `parse_nested_block` that can recurse; `depth` is 0 outside any bracket.
fn deeper<'i>(parser: &Parser<'i, '_>, depth: usize) -> Result<usize, Failure<'i>> {
    if depth >= MAX_NESTING {
        return Err(reject(
            parser,
            crate::i18n::current().settings_css_nesting(MAX_NESTING),
        ));
    }
    Ok(depth + 1)
}

/// Validate a custom style. An empty string (only whitespace) also complies and is treated as "no style" on save.
pub fn validate(css: &str) -> Result<(), Rejection> {
    if css.len() > MAX_BYTES {
        return Err(Rejection {
            line: None,
            reason: crate::i18n::current()
                .settings_css_too_long(MAX_BYTES / 1024)
                .into(),
        });
    }
    let line_of = |index: usize| Some(css[..index].matches('\n').count() as u32 + 1);
    if let Some(index) = css.find('\\') {
        return Err(Rejection {
            line: line_of(index),
            reason: crate::i18n::current().settings_css_backslash().into(),
        });
    }
    if let Some((index, _)) = css
        .char_indices()
        .find(|(_, c)| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err(Rejection {
            line: line_of(index),
            reason: crate::i18n::current().settings_css_control().into(),
        });
    }
    let mut input = ParserInput::new(css);
    let mut parser = Parser::new(&mut input);
    let mut rules = 0;
    stylesheet(&mut parser, false, &mut rules).map_err(|error| Rejection {
        line: Some(error.location.line + 1),
        reason: match error.kind {
            ParseErrorKind::Custom(reason) => reason,
            ParseErrorKind::Basic(_) => crate::i18n::current().settings_css_syntax().into(),
        },
    })
}

fn is_end<'i>(result: &Result<&Token<'i>, cssparser::BasicParseError<'i>>) -> bool {
    matches!(
        result,
        Err(cssparser::BasicParseError {
            kind: cssparser::BasicParseErrorKind::EndOfInput,
            ..
        })
    )
}

/// A rule list: at the top level or inside an `@media` block.
fn stylesheet<'i>(
    parser: &mut Parser<'i, '_>,
    nested: bool,
    rules: &mut usize,
) -> Result<(), Failure<'i>> {
    loop {
        let next = parser.next();
        if is_end(&next) {
            return Ok(());
        }
        let token = next?.clone();
        match token {
            Token::AtKeyword(name) => {
                if nested || !name.eq_ignore_ascii_case("media") {
                    return Err(reject(
                        parser,
                        if nested {
                            crate::i18n::current().settings_css_nested_at().to_owned()
                        } else {
                            crate::i18n::current().settings_css_at_rule(&*name)
                        },
                    ));
                }
                loop {
                    let token = parser.next_including_whitespace_and_comments()?.clone();
                    if token == Token::CurlyBracketBlock {
                        break;
                    }
                    media_token(parser, &token, 0)?;
                }
                // Recurses at most once: a nested stylesheet rejects every at-rule.
                parser.parse_nested_block(|parser| stylesheet(parser, true, rules))?;
            }
            first => {
                *rules += 1;
                if *rules > MAX_RULES {
                    return Err(reject(
                        parser,
                        crate::i18n::current().settings_css_rules(MAX_RULES),
                    ));
                }
                selector_token(parser, &first, 0)?;
                loop {
                    let token = parser.next_including_whitespace_and_comments()?.clone();
                    if token == Token::CurlyBracketBlock {
                        break;
                    }
                    selector_token(parser, &token, 0)?;
                }
                parser.parse_nested_block(declarations)?;
            }
        }
    }
}

fn media_token<'i>(
    parser: &mut Parser<'i, '_>,
    token: &Token<'i>,
    depth: usize,
) -> Result<(), Failure<'i>> {
    match token {
        Token::WhiteSpace(_)
        | Token::Comment(_)
        | Token::Ident(_)
        | Token::Comma
        | Token::Colon
        | Token::Number { .. }
        | Token::Dimension { .. }
        | Token::Percentage { .. }
        | Token::Delim('<' | '>' | '=' | '/') => Ok(()),
        Token::ParenthesisBlock => {
            let depth = deeper(parser, depth)?;
            parser.parse_nested_block(|parser| {
                loop {
                    let next = parser.next_including_whitespace_and_comments();
                    if is_end(&next) {
                        return Ok(());
                    }
                    let token = next?.clone();
                    media_token(parser, &token, depth)?;
                }
            })
        }
        _ => Err(reject(
            parser,
            crate::i18n::current().settings_css_media_condition(),
        )),
    }
}

/// Pseudo-class functions allowed in selectors.
const SELECTOR_FUNCTIONS: &[&str] = &[
    "not",
    "is",
    "where",
    "has",
    "nth-child",
    "nth-last-child",
    "nth-of-type",
    "nth-last-of-type",
    "lang",
    "dir",
];

fn selector_token<'i>(
    parser: &mut Parser<'i, '_>,
    token: &Token<'i>,
    depth: usize,
) -> Result<(), Failure<'i>> {
    match token {
        Token::WhiteSpace(_)
        | Token::Comment(_)
        | Token::Ident(_)
        | Token::Hash(_)
        | Token::IDHash(_)
        | Token::Colon
        | Token::Comma
        | Token::Number { .. }
        | Token::Dimension { .. }
        | Token::Delim('.' | '*' | '>' | '+' | '~' | '|' | '&' | '-') => Ok(()),
        Token::SquareBracketBlock => parser.parse_nested_block(|parser| {
            loop {
                let next = parser.next_including_whitespace_and_comments();
                if is_end(&next) {
                    return Ok(());
                }
                match next?.clone() {
                    Token::WhiteSpace(_)
                    | Token::Comment(_)
                    | Token::Ident(_)
                    | Token::QuotedString(_)
                    | Token::Delim('=' | '|' | '*')
                    | Token::IncludeMatch
                    | Token::DashMatch
                    | Token::PrefixMatch
                    | Token::SuffixMatch
                    | Token::SubstringMatch => {}
                    _ => {
                        return Err(reject(
                            parser,
                            crate::i18n::current().settings_css_attribute_selector(),
                        ));
                    }
                }
            }
        }),
        Token::Function(name)
            if SELECTOR_FUNCTIONS
                .iter()
                .any(|allowed| name.eq_ignore_ascii_case(allowed)) =>
        {
            let depth = deeper(parser, depth)?;
            parser.parse_nested_block(|parser| {
                loop {
                    let next = parser.next_including_whitespace_and_comments();
                    if is_end(&next) {
                        return Ok(());
                    }
                    let token = next?.clone();
                    selector_token(parser, &token, depth)?;
                }
            })
        }
        Token::Function(name) => Err(reject(
            parser,
            crate::i18n::current().settings_css_selector_function(name.to_string()),
        )),
        Token::CurlyBracketBlock | Token::Semicolon => Err(reject(
            parser,
            crate::i18n::current().settings_css_no_selector(),
        )),
        _ => Err(reject(
            parser,
            crate::i18n::current().settings_css_selector(),
        )),
    }
}

/// A declaration block: a sequence of `property: value;`. CSS nested rules are not supported.
fn declarations<'i>(parser: &mut Parser<'i, '_>) -> Result<(), Failure<'i>> {
    loop {
        let next = parser.next();
        if is_end(&next) {
            return Ok(());
        }
        match next?.clone() {
            Token::Semicolon => {}
            Token::Ident(name) => {
                let property = name.to_string();
                parser.parse_until_after(cssparser::Delimiter::Semicolon, |parser| {
                    parser.expect_colon()?;
                    declaration(parser, &property)
                })?;
            }
            _ => {
                return Err(reject(
                    parser,
                    crate::i18n::current().settings_css_expected_property(),
                ));
            }
        }
    }
}

/// Properties never allowed: old IE and old Firefox could use them to run scripts or load external bindings.
const FORBIDDEN_PROPERTIES: &[&str] = &["behavior", "-ms-behavior", "-moz-binding", "binding"];

/// A "meaningful" token of a value, for the disguise protection to check per property.
#[derive(Debug, PartialEq)]
enum Part {
    Ident(String),
    EmptyString,
    Integer(i32),
    Other,
}

fn declaration<'i>(parser: &mut Parser<'i, '_>, property: &str) -> Result<(), Failure<'i>> {
    let lower = property.to_ascii_lowercase();
    if FORBIDDEN_PROPERTIES.contains(&lower.as_str()) {
        return Err(reject(
            parser,
            crate::i18n::current().settings_css_property(property),
        ));
    }
    let mut parts = Vec::new();
    let mut important = false;
    loop {
        let next = parser.next_including_whitespace_and_comments();
        if is_end(&next) {
            break;
        }
        let token = next?.clone();
        if important {
            return Err(reject(
                parser,
                crate::i18n::current().settings_css_important_last(),
            ));
        }
        match token {
            Token::WhiteSpace(_) | Token::Comment(_) => {}
            Token::Delim('!') => {
                match parser.next()? {
                    Token::Ident(word) if word.eq_ignore_ascii_case("important") => {}
                    _ => return Err(reject(parser, crate::i18n::current().settings_css_bang())),
                }
                important = true;
            }
            Token::Ident(word) => parts.push(Part::Ident(word.to_ascii_lowercase())),
            Token::QuotedString(text) if text.is_empty() => parts.push(Part::EmptyString),
            Token::Number {
                int_value: Some(value),
                ..
            } => parts.push(Part::Integer(value)),
            other => {
                value_token(parser, &other, 0)?;
                parts.push(Part::Other);
            }
        }
    }
    if parts.is_empty() {
        return Err(reject(
            parser,
            crate::i18n::current().settings_css_no_value(property),
        ));
    }
    guard_disguise(parser, &lower, &parts)
}

/// Disguise protection. These three properties only accept literals; `var()` and the like are never
/// allowed, to prevent bypasses.
fn guard_disguise<'i>(
    parser: &Parser<'i, '_>,
    property: &str,
    parts: &[Part],
) -> Result<(), Failure<'i>> {
    let single = match parts {
        [part] => Some(part),
        _ => None,
    };
    match property {
        "position" => match single {
            Some(Part::Ident(word))
                if matches!(
                    word.as_str(),
                    "static"
                        | "relative"
                        | "absolute"
                        | "sticky"
                        | "initial"
                        | "inherit"
                        | "unset"
                        | "revert"
                        | "revert-layer"
                ) =>
            {
                Ok(())
            }
            _ => Err(reject(
                parser,
                crate::i18n::current().settings_css_position(),
            )),
        },
        "content" => match single {
            Some(Part::EmptyString) => Ok(()),
            Some(Part::Ident(word)) if matches!(word.as_str(), "none" | "normal") => Ok(()),
            _ => Err(reject(
                parser,
                crate::i18n::current().settings_css_content(),
            )),
        },
        "z-index" => match single {
            Some(Part::Ident(word)) if word == "auto" => Ok(()),
            Some(Part::Integer(value)) if (-1..=20).contains(value) => Ok(()),
            _ => Err(reject(
                parser,
                crate::i18n::current().settings_css_z_index(),
            )),
        },
        _ => Ok(()),
    }
}

/// Functions allowed in values: colors, variables and calculations, gradients, transforms, filters, easing, grids
/// and shapes. None of them issues a request.
const VALUE_FUNCTIONS: &[&str] = &[
    "rgb",
    "rgba",
    "hsl",
    "hsla",
    "hwb",
    "lab",
    "lch",
    "oklab",
    "oklch",
    "color",
    "color-mix",
    "light-dark",
    "var",
    "calc",
    "min",
    "max",
    "clamp",
    "linear-gradient",
    "radial-gradient",
    "conic-gradient",
    "repeating-linear-gradient",
    "repeating-radial-gradient",
    "repeating-conic-gradient",
    "translate",
    "translatex",
    "translatey",
    "translate3d",
    "rotate",
    "rotatex",
    "rotatey",
    "rotatez",
    "rotate3d",
    "scale",
    "scalex",
    "scaley",
    "scale3d",
    "skew",
    "skewx",
    "skewy",
    "matrix",
    "matrix3d",
    "perspective",
    "cubic-bezier",
    "steps",
    "blur",
    "brightness",
    "contrast",
    "drop-shadow",
    "grayscale",
    "hue-rotate",
    "invert",
    "opacity",
    "saturate",
    "sepia",
    "minmax",
    "repeat",
    "fit-content",
    "inset",
    "circle",
    "ellipse",
    "polygon",
];

fn value_token<'i>(
    parser: &mut Parser<'i, '_>,
    token: &Token<'i>,
    depth: usize,
) -> Result<(), Failure<'i>> {
    match token {
        Token::WhiteSpace(_)
        | Token::Comment(_)
        | Token::Ident(_)
        | Token::Hash(_)
        | Token::IDHash(_)
        | Token::QuotedString(_)
        | Token::Number { .. }
        | Token::Percentage { .. }
        | Token::Dimension { .. }
        | Token::Comma
        | Token::Delim('/' | '+' | '-' | '*' | '%' | '.') => Ok(()),
        Token::UnquotedUrl(_) | Token::BadUrl(_) => {
            Err(reject(parser, crate::i18n::current().settings_css_url()))
        }
        Token::Function(name) if name.eq_ignore_ascii_case("url") => {
            Err(reject(parser, crate::i18n::current().settings_css_url()))
        }
        Token::Function(name)
            if VALUE_FUNCTIONS
                .iter()
                .any(|allowed| name.eq_ignore_ascii_case(allowed)) =>
        {
            nested_value(parser, depth)
        }
        Token::Function(name) => Err(reject(
            parser,
            crate::i18n::current().settings_css_function(name.to_string()),
        )),
        Token::ParenthesisBlock => nested_value(parser, depth),
        Token::AtKeyword(_) => Err(reject(
            parser,
            crate::i18n::current().settings_css_value_at(),
        )),
        Token::CurlyBracketBlock => Err(reject(
            parser,
            crate::i18n::current().settings_css_expected_property(),
        )),
        _ => Err(reject(parser, crate::i18n::current().settings_css_value())),
    }
}

fn nested_value<'i>(parser: &mut Parser<'i, '_>, depth: usize) -> Result<(), Failure<'i>> {
    let depth = deeper(parser, depth)?;
    parser.parse_nested_block(|parser| {
        loop {
            let next = parser.next_including_whitespace_and_comments();
            if is_end(&next) {
                return Ok(());
            }
            let token = next?.clone();
            value_token(parser, &token, depth)?;
        }
    })
}

/// The custom style in the database: text and version. `(None, 0)` when there is no preference row.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Stored {
    pub css: Option<String>,
    pub version: i64,
}

const SELECT: &str =
    "SELECT custom_css,custom_css_version FROM gonof_home_reading_preference WHERE uid=?";

fn decode(row: Option<db::DbRow>) -> anyhow::Result<Stored> {
    let Some(row) = row else {
        return Ok(Stored::default());
    };
    Ok(Stored {
        css: row.try_get::<Option<String>, _>("custom_css")?,
        version: row.try_get("custom_css_version")?,
    })
}

/// The member's own custom style. The session was already verified in the middleware; this only reads one row by
/// primary key.
pub async fn load(db: &Database, uid: i64) -> anyhow::Result<Stored> {
    decode(
        crate::db::query(SELECT)
            .bind(uid)
            .fetch_optional(db.executor())
            .await?,
    )
}

/// Save or reset (`css` is `None` or only whitespace). Rechecks the credentials and compares the version under the
/// member row lock; the version only grows, and it is also the `v` in the stylesheet URL, so saving again after a
/// reset never hits an old copy cached in the browser. Returns the new version.
pub async fn save(
    db: &Database,
    cache: &crate::cache::Cache,
    actor: Actor,
    css: Option<&str>,
    version: i64,
) -> Result<i64, ContentWriteError> {
    let css = css.filter(|css| !css.trim().is_empty());
    if let Some(css) = css
        && validate(css).is_err()
    {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().settings_error_style_invalid(),
        ));
    }
    if version < 0 {
        return Err(ContentWriteError::Validation(
            crate::i18n::current().settings_error_style_version(),
        ));
    }
    let mut tx = db.begin_write().await?;
    interaction::lock_actor(db, &mut tx, actor, false).await?;
    let current = decode(
        crate::db::query(SELECT)
            .bind(actor.uid)
            .fetch_optional(db.observe(&mut *tx))
            .await?,
    )?;
    if current.version != version {
        return Err(ContentWriteError::Conflict(
            crate::i18n::current().settings_error_style_conflict(),
        ));
    }
    if current.css.as_deref() == css {
        tx.commit().await?;
        crate::cache::member::invalidate(cache, actor.uid).await?;
        return Ok(current.version);
    }
    let next = version.checked_add(1).ok_or(ContentWriteError::Validation(
        crate::i18n::current().settings_error_style_version_range(),
    ))?;
    let now = crate::identity::now();
    crate::db::query("INSERT INTO gonof_home_reading_preference(uid,custom_css,custom_css_version,updated_at) VALUES(?,?,?,?) ON DUPLICATE KEY UPDATE custom_css=VALUES(custom_css),custom_css_version=VALUES(custom_css_version),updated_at=VALUES(updated_at)")
        .bind(actor.uid)
        .bind(css)
        .bind(next)
        .bind(now)
        .execute(db.observe(&mut *tx))
        .await?;
    // The log only records the version and byte count, never the style text.
    crate::db::query("INSERT INTO gonof_common_log(uid,operationuid,type,data,source,dateline) VALUES(?,?,'identity.custom_css',?,'web',?)")
        .bind(actor.uid)
        .bind(actor.uid)
        .bind(serde_json::json!({"version": next, "bytes": css.map_or(0, str::len)}).to_string())
        .bind(now)
        .execute(db.observe(&mut *tx))
        .await?;
    tx.commit().await?;
    crate::cache::member::invalidate(cache, actor.uid).await?;
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rejected(css: &str) -> String {
        validate(css).expect_err(css).to_string()
    }

    #[test]
    fn ordinary_styles_pass() {
        for css in [
            "",
            // A comment in Chinese: "\u{6211}\u{7684}\u{6837}\u{5f0f}" means "my style".
            "/* \u{6211}\u{7684}\u{6837}\u{5f0f} */\n.rows--topics .row-title { color: #8b5cf6; font-weight: 600 !important; }",
            ":root { --brand: oklch(55% 0.2 300); --r-sm: 0; }",
            ".avatar, .avatar--initials { border-radius: 50%; }",
            "@media (max-width: 760px) and (prefers-color-scheme: dark) { body { background: linear-gradient(90deg, rgb(0 0 0 / 50%), transparent); } }",
            ".floor-card:not(.is-op):nth-child(2n+1) > .row-body { margin: calc(1rem + 2px) 0; }",
            "a[href^=\"/space/\"] { text-decoration: underline; }",
            ".layout__aside { position: sticky; top: 0; z-index: 2; }",
            ".quote::before { content: \"\"; }",
            ".x { transform: translate(-50%, 0) rotate(3deg); transition: opacity .2s cubic-bezier(.2,0,0,1); }",
        ] {
            assert_eq!(validate(css), Ok(()), "{css}");
        }
    }

    #[test]
    fn external_resources_are_rejected() {
        for css in [
            "body { background: url(https://evil.example/a.png); }",
            "body { background: url(\"/static/img/x.png\"); }",
            "body { background-image: image-set(\"a.png\" 1x); }",
            "body { background: -webkit-image-set(\"a.png\" 1x); }",
            "@import url(https://evil.example/x.css);",
            "@import \"x.css\";",
            "@font-face { font-family: x; src: local(x); }",
            "@namespace svg \"http://www.w3.org/2000/svg\";",
            "@keyframes spin { to { transform: rotate(1turn); } }",
            "input[name=_csrf][value^=a] { background: url(//evil.example/a); }",
            "body { cursor: url(x.cur), auto; }",
            "body { --x: url(a); }",
        ] {
            let reason = rejected(css);
            assert!(
                reason.contains("url()") || reason.contains('@') || reason.contains("not allowed"),
                "{css} -> {reason}"
            );
        }
    }

    #[test]
    fn script_and_obfuscation_are_rejected() {
        assert!(rejected("body { width: expression(alert(1)); }").contains("expression()"));
        assert!(rejected("body { behavior: url(x.htc); }").contains("behavior"));
        assert!(rejected("body { -moz-binding: url(x.xml#a); }").contains("-moz-binding"));
        assert_eq!(
            rejected("a {\n  background: u\\72l(x);\n}"),
            "Line 2: Backslash escapes are not allowed"
        );
        assert!(rejected("a { color: red } </style><script>").contains("Line 1: "));
        assert!(
            rejected("a { color: red; .b { color: blue } }")
                .contains("nested rules are not supported")
        );
        assert!(rejected("a { color: red !imp }").contains("important"));
        assert!(rejected("a { color: env(safe-area-inset-top) }").contains("env()"));
        assert!(rejected("a\u{0} { color: red }").contains("Control characters are not allowed"));
    }

    #[test]
    fn disguise_is_rejected() {
        // The classic overlay: fixed + inset:0 + a high z-index covering the whole page.
        assert!(
            rejected("body::after { position: fixed; inset: 0; z-index: 9999; }")
                .contains("position: fixed")
        );
        assert!(rejected(".a { position: var(--p); }").contains("position"));
        assert!(rejected(".a { z-index: 9999; }").contains("z-index"));
        assert!(rejected(".a { z-index: 21; }").contains("z-index"));
        assert!(rejected(".a { z-index: var(--z); }").contains("z-index"));
        assert!(
            rejected("h1::before { content: \"System notice: enter your password\"; }")
                .contains("content")
        );
        assert!(validate("h1::before { content: attr(title); }").is_err());
        assert!(rejected(".a { content: var(--text); }").contains("content"));
        assert_eq!(validate(".a { z-index: -1; position: relative; }"), Ok(()));
    }

    #[test]
    fn size_and_rule_limits_hold() {
        let big = format!(".a{{color:red}}{}", " ".repeat(MAX_BYTES));
        assert_eq!(rejected(&big), "A personal style can be at most 16 KiB");
        let many = ".a{color:red}".repeat(MAX_RULES + 1);
        assert!(many.len() <= MAX_BYTES);
        assert!(rejected(&many).contains("At most 1,000 rules"));
        assert!(rejected("@media screen { @media print { a { color: red } } }").contains("@media"));
        assert!(rejected("@media (min-width: calc(1px)) { a { color: red } }").contains("@media"));
    }

    const TOO_DEEP: &str = "Brackets and functions can be nested at most 16 levels deep";

    #[test]
    fn nesting_stops_at_the_limit() {
        let styles = |depth: usize| {
            let close = ")".repeat(depth);
            [
                format!(
                    "@media {}min-width: 1px{close} {{ a {{ color: red }} }}",
                    "(".repeat(depth)
                ),
                format!("a{}.b{close} {{ color: red }}", ":is(".repeat(depth)),
                format!("a {{ width: {}1px{close}; }}", "calc(".repeat(depth)),
                format!("a {{ width: calc({}1px{close}; }}", "(".repeat(depth - 1)),
            ]
        };
        for css in styles(MAX_NESTING) {
            assert_eq!(validate(&css), Ok(()), "{css}");
        }
        for css in styles(MAX_NESTING + 1) {
            assert_eq!(rejected(&css), format!("Line 1: {TOO_DEEP}"), "{css}");
        }
        // The line is the one with the first bracket past the limit.
        let css = format!(
            "a {{\n  width: {}1px{};\n}}",
            "(\n".repeat(20),
            ")".repeat(20)
        );
        assert_eq!(
            rejected(&css),
            format!("Line {}: {TOO_DEEP}", MAX_NESTING + 2)
        );
    }

    /// A whole style of opening brackets is refused at the limit rather than recursed into level by level, which
    /// would overflow the stack and abort the process.
    #[test]
    fn deep_nesting_floods_are_rejected_without_recursing() {
        let room = MAX_BYTES - 32;
        for css in [
            format!("a {{ width: {}", "(".repeat(room)),
            format!("a {{ width: {}", "calc(".repeat(room / 5)),
            format!("a {{ width: {}", "var(".repeat(room / 4)),
            format!("a{} {{ color: red }}", ":not(".repeat(room / 5)),
            format!("a {} {{ color: red }}", "is(".repeat(room / 3)),
            format!("@media {}", "(".repeat(room)),
            format!(
                "@media screen {{ a {{ width: {}{} }} }}",
                "(".repeat(room / 2),
                ")".repeat(room / 2)
            ),
        ] {
            assert!(css.len() <= MAX_BYTES);
            assert_eq!(
                rejected(&css),
                format!("Line 1: {TOO_DEEP}"),
                "{}",
                &css[..40]
            );
        }
    }
}
