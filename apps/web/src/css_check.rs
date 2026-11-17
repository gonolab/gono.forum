//! The restricted CSS checker shared by member personal styles ([`crate::custom_css`]) and skin stylesheets
//! ([`crate::skins`]); see docs/skins.md, "The stylesheet checker".
//!
//! Both profiles share the safety rules: nothing that can issue a request (`url()`, `image-set()`, `@import`,
//! `@font-face` and every at-rule other than `@media`), no backslash escapes or control characters, an allowlist of
//! functions in selectors and values, no `position: fixed`, `content` only empty, `none` or `normal`, `z-index` only
//! `auto` or -1 to 20 (the header is 30), and bracket nesting bounded so the recursive walk cannot overflow the stack.
//!
//! - **Member styles** stop at the first problem, which is reported as the reason; `!important` is allowed.
//! - **Skins** are checked to the end and every problem is listed. On top of the shared rules, two kinds of rules
//!   apply. Rules that block every skin: no `!important` (core keeps its guards with it, docs/skins.md, "Cascade
//!   layers"), no core-reserved `--gonof-*` variables, and the "Built on Gono Forum" guard (no selector naming
//!   `colophon`, and none of the guarded properties on `html`, `body`, `footer`, `:root` or `*`). Rules that block
//!   built-in skins and only warn for imported and instance skins: the header size guard, visible focus, actions that
//!   must stay visible, the six breakpoints, logical properties, the fixed values of docs/styles.md and selectors that
//!   depend on `aria-label`, `title` or child positions.
use cssparser::{ParseError, ParseErrorKind, Parser, ParserInput, ToCss, Token};
use serde::{Deserialize, Serialize};

/// How deeply brackets and functions may nest inside one `@media` condition, selector or value. Real styles stay
/// within a few levels (`color-mix(in oklch, var(--a, rgb(0 0 0 / calc(…))), …)` is four). The checker recurses
/// once per level and runs on the request's worker thread, so without this limit a style of nothing but `(` (about
/// 16,000 levels within 16 KiB) overflows the stack and aborts the whole process.
pub const MAX_NESTING: usize = 16;

/// At most this many problems are listed for a skin; the rest are summed up in one more line.
const MAX_FINDINGS: usize = 100;

/// Who wrote the stylesheet, which decides the rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// A member's personal style: shared rules only, first problem wins.
    Member,
    /// A skin stylesheet.
    Skin(Trust),
}

/// Built-in skins ship with the core and follow every style rule; imported and instance skins are the
/// administrator's choice and only get warnings for the rules that are not about safety (docs/skins.md, "Two kinds
/// of rules").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trust {
    Builtin,
    External,
}

impl Trust {
    /// The severity of a rule that blocks built-in skins and warns for the others.
    pub fn style(self) -> Severity {
        match self {
            Self::Builtin => Severity::Error,
            Self::External => Severity::Warning,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

/// One problem: where, how bad, and what. Stored as a code with its values (the warnings an administrator confirmed
/// are kept in `gonof_common_skin.warnings`) and worded in the reader's language when shown.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// Which file of a skin: `skin.toml`, `palettes/<name>.json` or `skin.css`; empty for member styles.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub file: String,
    /// From 1; `None` for problems not tied to a line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    pub severity: Severity,
    #[serde(flatten)]
    pub problem: Problem,
}

impl Finding {
    pub fn text(&self, t: crate::i18n::Lang) -> String {
        let reason = self.problem.text(t);
        let located = match self.line {
            Some(line) => t.settings_css_line(line.to_string(), reason.as_str()),
            None => reason,
        };
        if self.file.is_empty() {
            located
        } else {
            t.skin_finding_file(self.file.as_str(), located.as_str())
        }
    }
}

/// What is wrong. The variants up to `Value` are the shared rules; the rest only apply to skins.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "kebab-case")]
pub enum Problem {
    TooLong {
        kib: usize,
        skin: bool,
    },
    Backslash,
    Control,
    Syntax,
    Nesting,
    NestedAt,
    AtRule {
        name: String,
    },
    Rules {
        max: usize,
    },
    MediaCondition,
    AttributeSelector,
    SelectorFunction {
        name: String,
    },
    NoSelector,
    Selector,
    ExpectedProperty,
    Property {
        property: String,
    },
    ImportantLast,
    Bang,
    NoValue {
        property: String,
    },
    Position,
    Content,
    ZIndex,
    Url,
    Function {
        name: String,
    },
    ValueAt,
    Value,
    /// More problems than are listed.
    More {
        count: usize,
    },
    // ---- skins: every skin ----
    Important,
    ReservedVariable {
        name: String,
    },
    Colophon,
    GuardedRoot {
        property: String,
    },
    // ---- skins: built-in blocks, external warns ----
    MastheadSize {
        property: String,
    },
    FocusHidden,
    ActionsHidden {
        property: String,
    },
    Breakpoint {
        condition: String,
    },
    Physical {
        property: String,
    },
    FixedValue {
        property: String,
        value: String,
    },
    DesignVariable {
        name: String,
    },
    FragileSelector,
    // ---- skins: manifest, palettes and packs ----
    ManifestMissing,
    ManifestSyntax {
        detail: String,
    },
    ManifestTooLong {
        kib: usize,
    },
    ManifestContract {
        found: String,
    },
    ManifestId {
        value: String,
    },
    ManifestDirectory {
        id: String,
        directory: String,
    },
    ManifestVersion {
        field: String,
    },
    ManifestName {
        field: String,
    },
    ManifestText {
        field: String,
    },
    ManifestValue {
        field: String,
    },
    ManifestUnsupported {
        field: String,
    },
    /// `frame.nav = "side"` without blocks in the `side.nav` place of the skin's `[layout]`.
    ManifestSideNav,
    /// The manifest's `[layout]` breaks a rule of layouts (docs/skins.md, "Places and blocks"): the field's path
    /// and the problem's code.
    ManifestLayout {
        path: String,
        problem: String,
    },
    ManifestMinForum {
        version: String,
    },
    ManifestBuiltinText {
        field: String,
    },
    ManifestBuiltinMessages {
        id: String,
    },
    PaletteSyntax {
        detail: String,
    },
    PaletteTooLong {
        kib: usize,
    },
    PaletteMissing {
        name: String,
    },
    PaletteUnused {
        name: String,
    },
    PaletteToken {
        name: String,
    },
    PaletteValue {
        name: String,
    },
    PaletteUnknown {
        name: String,
    },
    PaletteReference {
        mode: String,
        name: String,
        target: String,
    },
    PaletteCycle {
        mode: String,
        chain: String,
    },
    Contrast {
        mode: String,
        foreground: String,
        background: String,
        ratio: String,
        min: String,
    },
    ContrastUnknown {
        mode: String,
        foreground: String,
        background: String,
    },
    ColophonColor {
        mode: String,
    },
    ColophonContrast {
        mode: String,
        foreground: String,
        ratio: String,
    },
    PackFile,
    PackTooLong {
        kib: usize,
    },
    PackSyntax {
        detail: String,
    },
    PackFormat,
}

impl Problem {
    pub fn text(&self, t: crate::i18n::Lang) -> String {
        match self {
            Self::TooLong { kib, skin: false } => t.settings_css_too_long(*kib),
            Self::TooLong { kib, skin: true } => t.skin_css_too_long(*kib),
            Self::Backslash => t.settings_css_backslash().into(),
            Self::Control => t.settings_css_control().into(),
            Self::Syntax => t.settings_css_syntax().into(),
            Self::Nesting => t.settings_css_nesting(MAX_NESTING),
            Self::NestedAt => t.settings_css_nested_at().into(),
            Self::AtRule { name } => t.settings_css_at_rule(name.as_str()),
            Self::Rules { max } => t.settings_css_rules(*max),
            Self::MediaCondition => t.settings_css_media_condition().into(),
            Self::AttributeSelector => t.settings_css_attribute_selector().into(),
            Self::SelectorFunction { name } => t.settings_css_selector_function(name.as_str()),
            Self::NoSelector => t.settings_css_no_selector().into(),
            Self::Selector => t.settings_css_selector().into(),
            Self::ExpectedProperty => t.settings_css_expected_property().into(),
            Self::Property { property } => t.settings_css_property(property.as_str()),
            Self::ImportantLast => t.settings_css_important_last().into(),
            Self::Bang => t.settings_css_bang().into(),
            Self::NoValue { property } => t.settings_css_no_value(property.as_str()),
            Self::Position => t.settings_css_position().into(),
            Self::Content => t.settings_css_content().into(),
            Self::ZIndex => t.settings_css_z_index().into(),
            Self::Url => t.settings_css_url().into(),
            Self::Function { name } => t.settings_css_function(name.as_str()),
            Self::ValueAt => t.settings_css_value_at().into(),
            Self::Value => t.settings_css_value().into(),
            Self::More { count } => t.skin_more_findings(*count),
            Self::Important => t.skin_css_important().into(),
            Self::ReservedVariable { name } => t.skin_css_reserved_variable(name.as_str()),
            Self::Colophon => t.skin_css_colophon().into(),
            Self::GuardedRoot { property } => t.skin_css_guarded_root(property.as_str()),
            Self::MastheadSize { property } => t.skin_css_masthead_size(property.as_str()),
            Self::FocusHidden => t.skin_css_focus_hidden().into(),
            Self::ActionsHidden { property } => t.skin_css_actions_hidden(property.as_str()),
            Self::Breakpoint { condition } => t.skin_css_breakpoint(condition.as_str()),
            Self::Physical { property } => t.skin_css_physical(property.as_str()),
            Self::FixedValue { property, value } => {
                t.skin_css_fixed_value(property.as_str(), value.as_str())
            }
            Self::DesignVariable { name } => t.skin_css_design_variable(name.as_str()),
            Self::FragileSelector => t.skin_css_fragile_selector().into(),
            Self::ManifestMissing => t.skin_manifest_missing().into(),
            Self::ManifestSyntax { detail } => t.skin_manifest_syntax(detail.as_str()),
            Self::ManifestTooLong { kib } => t.skin_manifest_too_long(*kib),
            Self::ManifestContract { found } => {
                t.skin_manifest_contract(found.as_str(), crate::skins::manifest::CONTRACT)
            }
            Self::ManifestId { value } => t.skin_manifest_id(value.as_str()),
            Self::ManifestDirectory { id, directory } => {
                t.skin_manifest_directory(directory.as_str(), id.as_str())
            }
            Self::ManifestVersion { field } => t.skin_manifest_version(field.as_str()),
            Self::ManifestName { field } => t.skin_manifest_name(field.as_str()),
            Self::ManifestText { field } => t.skin_manifest_text(field.as_str()),
            Self::ManifestValue { field } => t.skin_manifest_value(field.as_str()),
            Self::ManifestUnsupported { field } => t.skin_manifest_unsupported(field.as_str()),
            Self::ManifestSideNav => t.skin_manifest_side_nav().into(),
            Self::ManifestLayout { path, problem } => t.skin_manifest_layout(
                crate::layout::LayoutError::from_code(path, problem).message(t),
            ),
            Self::ManifestMinForum { version } => {
                t.skin_manifest_min_forum(version.as_str(), crate::VERSION)
            }
            Self::ManifestBuiltinText { field } => t.skin_manifest_builtin_text(field.as_str()),
            Self::ManifestBuiltinMessages { id } => t.skin_manifest_builtin_messages(id.as_str()),
            Self::PaletteSyntax { detail } => t.skin_palette_syntax(detail.as_str()),
            Self::PaletteTooLong { kib } => t.skin_palette_too_long(*kib),
            Self::PaletteMissing { name } => t.skin_palette_missing(name.as_str()),
            Self::PaletteUnused { name } => t.skin_palette_unused(name.as_str()),
            Self::PaletteToken { name } => t.skin_palette_token(name.as_str()),
            Self::PaletteValue { name } => t.skin_palette_value(name.as_str()),
            Self::PaletteUnknown { name } => t.skin_palette_unknown(name.as_str()),
            Self::PaletteReference { mode, name, target } => {
                t.skin_palette_reference(mode_word(t, mode), name.as_str(), target.as_str())
            }
            Self::PaletteCycle { mode, chain } => {
                t.skin_palette_cycle(mode_word(t, mode), chain.as_str())
            }
            Self::Contrast {
                mode,
                foreground,
                background,
                ratio,
                min,
            } => t.skin_contrast(
                mode_word(t, mode),
                foreground.as_str(),
                background.as_str(),
                ratio.as_str(),
                min.as_str(),
            ),
            Self::ContrastUnknown {
                mode,
                foreground,
                background,
            } => t.skin_contrast_unknown(
                mode_word(t, mode),
                foreground.as_str(),
                background.as_str(),
            ),
            Self::ColophonColor { mode } => t.skin_colophon_color(mode_word(t, mode)),
            Self::ColophonContrast {
                mode,
                foreground,
                ratio,
            } => t.skin_colophon_contrast(mode_word(t, mode), foreground.as_str(), ratio.as_str()),
            Self::PackFile => t.skin_pack_file().into(),
            Self::PackTooLong { kib } => t.skin_pack_too_long(*kib),
            Self::PackSyntax { detail } => t.skin_pack_syntax(detail.as_str()),
            Self::PackFormat => t.skin_pack_format().into(),
        }
    }
}

/// "light mode" or "dark mode" in the reader's language.
fn mode_word(t: crate::i18n::Lang, mode: &str) -> &'static str {
    if mode == "dark" {
        t.skin_mode_dark()
    } else {
        t.skin_mode_light()
    }
}

/// Checks a stylesheet. `max_bytes` and `max_rules` are the profile's limits. Member styles return at most one
/// finding, the first error; skins return every finding (errors and warnings) in source order.
pub fn check(css: &str, profile: Profile, max_bytes: usize, max_rules: usize) -> Vec<Finding> {
    let error = |line, problem| Finding {
        file: String::new(),
        line,
        severity: Severity::Error,
        problem,
    };
    if css.len() > max_bytes {
        return vec![error(
            None,
            Problem::TooLong {
                kib: max_bytes / 1024,
                skin: profile != Profile::Member,
            },
        )];
    }
    let line_of = |index: usize| Some(css[..index].matches('\n').count() as u32 + 1);
    if let Some(index) = css.find('\\') {
        return vec![error(line_of(index), Problem::Backslash)];
    }
    if let Some((index, _)) = css
        .char_indices()
        .find(|(_, c)| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return vec![error(line_of(index), Problem::Control)];
    }
    let mut input = ParserInput::new(css);
    let mut parser = Parser::new(&mut input);
    let mut walk = Walk {
        profile,
        max_rules,
        rules: 0,
        findings: Vec::new(),
        dropped: 0,
        in_prelude: false,
    };
    if let Err(error) = walk.stylesheet(&mut parser, false) {
        walk.record(error);
    }
    let mut findings = walk.findings;
    if walk.dropped > 0 {
        findings.push(Finding {
            file: String::new(),
            line: None,
            severity: Severity::Error,
            problem: Problem::More {
                count: walk.dropped,
            },
        });
    }
    findings
}

/// Boxed, so a parse result stays small on the recursive walk.
type Failure<'i> = ParseError<'i, Box<Problem>>;

fn reject<'i>(parser: &Parser<'i, '_>, problem: Problem) -> Failure<'i> {
    parser.new_custom_error(Box::new(problem))
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

/// The depth inside the block the parser is about to enter, or a rejection past [`MAX_NESTING`]. Called before
/// every `parse_nested_block` that can recurse; `depth` is 0 outside any bracket.
fn deeper<'i>(parser: &Parser<'i, '_>, depth: usize) -> Result<usize, Failure<'i>> {
    if depth >= MAX_NESTING {
        return Err(reject(parser, Problem::Nesting));
    }
    Ok(depth + 1)
}

struct Walk {
    profile: Profile,
    max_rules: usize,
    rules: usize,
    findings: Vec<Finding>,
    /// Findings past [`MAX_FINDINGS`], only counted.
    dropped: usize,
    /// The parser is inside a rule's prelude (selector or `@media` condition): after an error the rest of the rule,
    /// block included, is skipped.
    in_prelude: bool,
}

/// What a rule's selector says about the elements it can match, for the skin rules. Worked out from the selector's
/// source text, which the shared rules have already limited to plain tokens.
#[derive(Default)]
struct Selector {
    /// Names `colophon` anywhere: the footer's brand line and its parts.
    colophon: bool,
    /// The subject of one of its complex selectors can be `html`, `body`, `footer`, `:root` or every element.
    frame_root: bool,
    /// Names a header part whose sizes `router/nav_fit.rs` measured: `masthead*`, `forum-more*`, `theme-switch*` or
    /// `container`.
    masthead: bool,
    /// Names the floor actions (`actions-bar`).
    actions: bool,
    /// Has a `:focus…` pseudo-class.
    focus: bool,
    /// Depends on `aria-label`, `title` or the element's position among its siblings.
    fragile: bool,
    /// The subject can be the page root or body: the place for the body text size.
    body_text: bool,
}

impl Selector {
    fn of(text: &str) -> Self {
        let lower = text.to_ascii_lowercase();
        let mut facts = Self {
            colophon: lower.contains("colophon"),
            actions: lower.contains("actions-bar"),
            focus: lower.contains(":focus"),
            fragile: [
                "[aria-label",
                "[title",
                ":nth-",
                ":first-child",
                ":last-child",
                ":only-child",
                ":first-of-type",
                ":last-of-type",
                ":only-of-type",
            ]
            .iter()
            .any(|needle| lower.contains(needle)),
            ..Self::default()
        };
        facts.masthead = words(&lower).any(|(prefix, word)| {
            matches!(prefix, '.' | '=' | '"' | '\'' | '*')
                && (word.starts_with("masthead")
                    || word.starts_with("forum-more")
                    || word.starts_with("theme-switch")
                    || word == "container")
        });
        for complex in split_top(&lower, ',') {
            let complex = complex.trim();
            let subject = subject_of(complex);
            let types: Vec<&str> = type_selectors(subject).collect();
            // `*` matches the frame when nothing before it rules the frame out: alone, or below `html`, `body`,
            // `footer` or `:root`.
            let ancestors = &complex[..complex.len() - subject.len()];
            let universal = subject.starts_with('*')
                && (ancestors.trim().is_empty()
                    || ancestors.contains(":root")
                    || type_selectors(ancestors).any(|t| matches!(t, "html" | "body" | "footer")));
            let root = subject.contains(":root") || universal;
            if root
                || types
                    .iter()
                    .any(|t| matches!(*t, "html" | "body" | "footer"))
            {
                facts.frame_root = true;
            }
            if root || types.iter().any(|t| matches!(*t, "html" | "body")) {
                facts.body_text = true;
            }
        }
        facts
    }
}

/// The words of a selector (letters, digits, `-` and `_`) with the character before each.
fn words(text: &str) -> impl Iterator<Item = (char, &str)> {
    let mut found = Vec::new();
    let mut previous = ' ';
    let mut start = None;
    for (index, c) in text.char_indices() {
        let part = c.is_ascii_alphanumeric() || c == '-' || c == '_';
        match (part, start) {
            (true, None) => start = Some((index, previous)),
            (false, Some((from, before))) => {
                found.push((before, &text[from..index]));
                start = None;
            }
            _ => {}
        }
        previous = c;
    }
    if let Some((from, before)) = start {
        found.push((before, &text[from..]));
    }
    found.into_iter()
}

/// Splits at `separator` outside brackets and quotes.
fn split_top(text: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0_i32;
    let mut quote = None;
    let mut start = 0;
    for (index, c) in text.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '(' | '[') => depth += 1,
            (None, ')' | ']') => depth -= 1,
            (None, c) if c == separator && depth == 0 => {
                parts.push(&text[start..index]);
                start = index + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

/// The last compound selector of a complex selector: what is after the last combinator outside brackets.
fn subject_of(complex: &str) -> &str {
    let complex = complex.trim();
    let mut depth = 0_i32;
    let mut quote = None;
    let mut start = 0;
    for (index, c) in complex.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '(' | '[') => depth += 1,
            (None, ')' | ']') => depth -= 1,
            (None, c) if depth == 0 && (c.is_whitespace() || matches!(c, '>' | '+' | '~')) => {
                start = index + c.len_utf8();
            }
            _ => {}
        }
    }
    &complex[start..]
}

/// Type selectors in a compound selector, including those inside `:is()`, `:where()` and `:not()`: a word at the
/// start or after `(`, `,` or a space.
fn type_selectors(compound: &str) -> impl Iterator<Item = &str> {
    words(compound)
        .filter(|(before, _)| matches!(before, ' ' | '(' | ',' | '>' | '+' | '~'))
        .map(|(_, word)| word)
        .chain(
            compound
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
                .next()
                .filter(|first| !first.is_empty()),
        )
}

/// Properties the "Built on Gono Forum" guard fixes on `html`, `body` and the footer (docs/skins.md, "Built on Gono
/// Forum"): a skin cannot hide, shrink, cut off or move the page frame the footer line sits in.
const GUARDED_ROOT_PROPERTIES: &[&str] = &[
    "display",
    "visibility",
    "opacity",
    "filter",
    "backdrop-filter",
    "transform",
    "translate",
    "rotate",
    "scale",
    "zoom",
    "clip",
    "clip-path",
    "mask",
    "mask-image",
    "mask-size",
    "mask-position",
    "mask-repeat",
    "mask-mode",
    "mask-clip",
    "mask-origin",
    "mask-composite",
    "mask-border",
    "-webkit-mask",
    "-webkit-mask-image",
    "-webkit-mask-size",
    "-webkit-mask-position",
    "-webkit-mask-repeat",
    "-webkit-mask-box-image",
    "overflow",
    "overflow-x",
    "overflow-y",
    "overflow-block",
    "overflow-inline",
    "max-block-size",
    "max-height",
    "content-visibility",
];

/// Header sizes `router/nav_fit.rs` estimates widths from (docs/styles.md, "Header navigation widths").
fn masthead_size(property: &str) -> bool {
    property.starts_with("padding")
        || property.starts_with("margin")
        || matches!(
            property,
            "font-size"
                | "font"
                | "letter-spacing"
                | "word-spacing"
                | "gap"
                | "column-gap"
                | "row-gap"
                | "width"
                | "inline-size"
                | "min-width"
                | "min-inline-size"
                | "max-width"
                | "max-inline-size"
                | "flex-basis"
                | "border"
                | "border-width"
                | "border-inline"
                | "border-inline-width"
                | "border-inline-start"
                | "border-inline-end"
                | "border-inline-start-width"
                | "border-inline-end-width"
                | "border-left"
                | "border-right"
                | "border-left-width"
                | "border-right-width"
        )
}

/// Physical properties that have a flow-relative form (docs/styles.md, "Logical properties and right-to-left").
fn physical(property: &str) -> bool {
    const SIDES: [&str; 4] = ["left", "right", "top", "bottom"];
    if SIDES.contains(&property)
        || matches!(
            property,
            "width" | "height" | "min-width" | "min-height" | "max-width" | "max-height"
        )
    {
        return true;
    }
    for prefix in [
        "margin-",
        "padding-",
        "border-",
        "scroll-margin-",
        "scroll-padding-",
    ] {
        if let Some(rest) = property.strip_prefix(prefix) {
            let side = rest.split('-').next().unwrap_or("");
            if SIDES.contains(&side) {
                return true;
            }
        }
    }
    false
}

/// Design variables of docs/styles.md, "Design variables", and the values built-in skins may give them.
fn design_variable(name: &str) -> Option<&'static [&'static str]> {
    Some(match name {
        "--r-xs" | "--r-sm" | "--r-input" | "--r" => &["4px", "6px", "8px", "12px"],
        "--r-pill" => &["9999px"],
        "--container" => &["1200px"],
        "--sidebar-w" => &["260px", "220px"],
        "--side-nav-w" => &["200px"],
        "--gap" => &["20px", "16px"],
        "--post-image-max" => &["720px"],
        "--masthead-h" | "--sticky-top" | "--safe-top" | "--safe-right" | "--safe-bottom"
        | "--safe-left" => &[],
        _ => return None,
    })
}

impl Walk {
    fn severity(&self) -> Option<Severity> {
        match self.profile {
            Profile::Member => None,
            Profile::Skin(trust) => Some(trust.style()),
        }
    }

    fn push(&mut self, line: u32, severity: Severity, problem: Problem) {
        if self.findings.len() >= MAX_FINDINGS {
            self.dropped += 1;
            return;
        }
        self.findings.push(Finding {
            file: String::new(),
            line: Some(line),
            severity,
            problem,
        });
    }

    /// A style rule of a skin that blocks built-in skins and warns for the others; member styles skip it.
    fn style(&mut self, parser: &Parser<'_, '_>, problem: Problem) {
        if let Some(severity) = self.severity() {
            self.push(parser.current_source_location().line + 1, severity, problem);
        }
    }

    /// A rule that blocks every skin; member styles skip it.
    fn skin_error(&mut self, parser: &Parser<'_, '_>, problem: Problem) {
        if self.severity().is_some() {
            self.push(
                parser.current_source_location().line + 1,
                Severity::Error,
                problem,
            );
        }
    }

    fn record(&mut self, error: Failure<'_>) {
        let problem = match error.kind {
            ParseErrorKind::Custom(problem) => *problem,
            ParseErrorKind::Basic(_) => Problem::Syntax,
        };
        self.push(error.location.line + 1, Severity::Error, problem);
    }

    /// Member styles stop at the first error; skins record it and carry on.
    fn recover<'i>(&mut self, error: Failure<'i>) -> Result<(), Failure<'i>> {
        match self.profile {
            Profile::Member => Err(error),
            Profile::Skin(_) => {
                self.record(error);
                Ok(())
            }
        }
    }

    /// A rule list: at the top level or inside an `@media` block.
    fn stylesheet<'i>(
        &mut self,
        parser: &mut Parser<'i, '_>,
        nested: bool,
    ) -> Result<(), Failure<'i>> {
        loop {
            let next = parser.next();
            if is_end(&next) {
                return Ok(());
            }
            let token = next?.clone();
            self.in_prelude = true;
            if let Err(error) = self.rule(parser, token, nested) {
                let skip = self.in_prelude;
                self.recover(error)?;
                if skip {
                    // Skip the rest of the prelude and the block that follows it.
                    loop {
                        match parser.next_including_whitespace_and_comments() {
                            Ok(Token::CurlyBracketBlock | Token::Semicolon) | Err(_) => break,
                            Ok(_) => {}
                        }
                    }
                }
            }
            self.in_prelude = false;
        }
    }

    fn rule<'i>(
        &mut self,
        parser: &mut Parser<'i, '_>,
        first: Token<'i>,
        nested: bool,
    ) -> Result<(), Failure<'i>> {
        let start = parser.position();
        if matches!(first, Token::CurlyBracketBlock | Token::Semicolon) {
            // Nothing of the next rule has been read: there is nothing to skip.
            self.in_prelude = false;
            return Err(reject(parser, Problem::NoSelector));
        }
        match first {
            Token::AtKeyword(name) => {
                if nested || !name.eq_ignore_ascii_case("media") {
                    return Err(reject(
                        parser,
                        if nested {
                            Problem::NestedAt
                        } else {
                            Problem::AtRule {
                                name: name.to_string(),
                            }
                        },
                    ));
                }
                loop {
                    let token = parser.next_including_whitespace_and_comments()?.clone();
                    if token == Token::CurlyBracketBlock {
                        break;
                    }
                    self.media_token(parser, &token, 0)?;
                }
                if self.severity().is_some() {
                    let condition = parser.slice_from(start);
                    let condition = condition.trim_end_matches('{').trim().to_owned();
                    if !breakpoint_allowed(&condition) {
                        self.style(parser, Problem::Breakpoint { condition });
                    }
                }
                self.in_prelude = false;
                // Recurses at most once: a nested stylesheet rejects every at-rule.
                parser.parse_nested_block(|parser| self.stylesheet(parser, true))
            }
            first => {
                self.rules += 1;
                if self.rules > self.max_rules {
                    return Err(reject(
                        parser,
                        Problem::Rules {
                            max: self.max_rules,
                        },
                    ));
                }
                let before = parser.position();
                self.selector_token(parser, &first, 0)?;
                loop {
                    let token = parser.next_including_whitespace_and_comments()?.clone();
                    if token == Token::CurlyBracketBlock {
                        break;
                    }
                    self.selector_token(parser, &token, 0)?;
                }
                // `first` was read before `before`: put its text back in front.
                let text = format!(
                    "{}{}",
                    first.to_css_string(),
                    parser.slice_from(before).trim_end_matches('{')
                );
                let selector = Selector::of(&text);
                if self.severity().is_some() {
                    if selector.colophon {
                        self.skin_error(parser, Problem::Colophon);
                    }
                    if selector.fragile {
                        self.style(parser, Problem::FragileSelector);
                    }
                }
                self.in_prelude = false;
                parser.parse_nested_block(|parser| self.declarations(parser, &selector))
            }
        }
    }

    fn media_token<'i>(
        &mut self,
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
                        self.media_token(parser, &token, depth)?;
                    }
                })
            }
            _ => Err(reject(parser, Problem::MediaCondition)),
        }
    }

    fn selector_token<'i>(
        &mut self,
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
                        _ => return Err(reject(parser, Problem::AttributeSelector)),
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
                        self.selector_token(parser, &token, depth)?;
                    }
                })
            }
            Token::Function(name) => Err(reject(
                parser,
                Problem::SelectorFunction {
                    name: name.to_string(),
                },
            )),
            Token::CurlyBracketBlock | Token::Semicolon => Err(reject(parser, Problem::NoSelector)),
            _ => Err(reject(parser, Problem::Selector)),
        }
    }

    /// A declaration block: a sequence of `property: value;`. CSS nested rules are not supported.
    fn declarations<'i>(
        &mut self,
        parser: &mut Parser<'i, '_>,
        selector: &Selector,
    ) -> Result<(), Failure<'i>> {
        loop {
            let next = parser.next();
            if is_end(&next) {
                return Ok(());
            }
            match next?.clone() {
                Token::Semicolon => {}
                Token::Ident(name) => {
                    let property = name.to_string();
                    let result =
                        parser.parse_until_after(cssparser::Delimiter::Semicolon, |parser| {
                            parser.expect_colon()?;
                            self.declaration(parser, &property, selector)
                        });
                    if let Err(error) = result {
                        self.recover(error)?;
                    }
                }
                _ => {
                    let error = reject(parser, Problem::ExpectedProperty);
                    self.recover(error)?;
                    // Skip to the next declaration.
                    let _ = parser.parse_until_after(cssparser::Delimiter::Semicolon, |parser| {
                        while parser.next().is_ok() {}
                        Ok::<(), Failure<'i>>(())
                    });
                }
            }
        }
    }

    fn declaration<'i>(
        &mut self,
        parser: &mut Parser<'i, '_>,
        property: &str,
        selector: &Selector,
    ) -> Result<(), Failure<'i>> {
        let lower = property.to_ascii_lowercase();
        if FORBIDDEN_PROPERTIES.contains(&lower.as_str()) {
            return Err(reject(
                parser,
                Problem::Property {
                    property: property.to_owned(),
                },
            ));
        }
        let start = parser.position();
        let mut value_end = start;
        let mut parts = Vec::new();
        let mut important = false;
        loop {
            let next = parser.next_including_whitespace_and_comments();
            if is_end(&next) {
                break;
            }
            let token = next?.clone();
            if important {
                return Err(reject(parser, Problem::ImportantLast));
            }
            match token {
                Token::WhiteSpace(_) | Token::Comment(_) => {}
                Token::Delim('!') => {
                    match parser.next()? {
                        Token::Ident(word) if word.eq_ignore_ascii_case("important") => {}
                        _ => return Err(reject(parser, Problem::Bang)),
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
                    self.value_token(parser, &other, 0)?;
                    parts.push(Part::Other);
                }
            }
            if !important {
                value_end = parser.position();
            }
        }
        if parts.is_empty() {
            return Err(reject(
                parser,
                Problem::NoValue {
                    property: property.to_owned(),
                },
            ));
        }
        guard_disguise(parser, &lower, &parts)?;
        if self.severity().is_none() {
            return Ok(());
        }
        if important {
            self.skin_error(parser, Problem::Important);
        }
        let value = parser.slice(start..value_end).trim().to_ascii_lowercase();
        self.skin_rules(parser, &lower, &value, selector);
        Ok(())
    }

    /// The rules only skins have, for one declaration.
    fn skin_rules(
        &mut self,
        parser: &Parser<'_, '_>,
        property: &str,
        value: &str,
        selector: &Selector,
    ) {
        if property.starts_with("--gonof-") {
            self.skin_error(
                parser,
                Problem::ReservedVariable {
                    name: property.to_owned(),
                },
            );
        }
        if selector.frame_root && GUARDED_ROOT_PROPERTIES.contains(&property) {
            self.skin_error(
                parser,
                Problem::GuardedRoot {
                    property: property.to_owned(),
                },
            );
        }
        if selector.masthead && masthead_size(property) {
            self.style(
                parser,
                Problem::MastheadSize {
                    property: property.to_owned(),
                },
            );
        }
        if selector.focus
            && (matches!(property, "outline" | "outline-style" | "outline-width")
                && matches!(value, "none" | "0" | "0px" | "hidden"))
        {
            self.style(parser, Problem::FocusHidden);
        }
        if selector.actions && matches!(property, "opacity" | "visibility") {
            self.style(
                parser,
                Problem::ActionsHidden {
                    property: property.to_owned(),
                },
            );
        }
        if physical(property) {
            self.style(
                parser,
                Problem::Physical {
                    property: property.to_owned(),
                },
            );
        }
        if let Some(allowed) = design_variable(property)
            && !allowed.contains(&value)
        {
            self.style(
                parser,
                Problem::DesignVariable {
                    name: property.to_owned(),
                },
            );
        }
        if !fixed_value_allowed(property, value, selector) {
            self.style(
                parser,
                Problem::FixedValue {
                    property: property.to_owned(),
                    value: value.to_owned(),
                },
            );
        }
    }

    fn value_token<'i>(
        &mut self,
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
            Token::UnquotedUrl(_) | Token::BadUrl(_) => Err(reject(parser, Problem::Url)),
            Token::Function(name) if name.eq_ignore_ascii_case("url") => {
                Err(reject(parser, Problem::Url))
            }
            Token::Function(name)
                if VALUE_FUNCTIONS
                    .iter()
                    .any(|allowed| name.eq_ignore_ascii_case(allowed)) =>
            {
                self.nested_value(parser, depth)
            }
            Token::Function(name) => Err(reject(
                parser,
                Problem::Function {
                    name: name.to_string(),
                },
            )),
            Token::ParenthesisBlock => self.nested_value(parser, depth),
            Token::AtKeyword(_) => Err(reject(parser, Problem::ValueAt)),
            Token::CurlyBracketBlock => Err(reject(parser, Problem::ExpectedProperty)),
            _ => Err(reject(parser, Problem::Value)),
        }
    }

    fn nested_value<'i>(
        &mut self,
        parser: &mut Parser<'i, '_>,
        depth: usize,
    ) -> Result<(), Failure<'i>> {
        let depth = deeper(parser, depth)?;
        parser.parse_nested_block(|parser| {
            loop {
                let next = parser.next_including_whitespace_and_comments();
                if is_end(&next) {
                    return Ok(());
                }
                let token = next?.clone();
                self.value_token(parser, &token, depth)?;
            }
        })
    }
}

/// The widths docs/styles.md, "Breakpoints", allows, plus reduced motion and print.
const BREAKPOINTS: [&str; 6] = ["900px", "760px", "700px", "640px", "480px", "420px"];

/// Whether an `@media` condition (without `@media`) only uses the six widths as `max-width`, `prefers-reduced-motion`
/// and the media types.
fn breakpoint_allowed(condition: &str) -> bool {
    let condition = condition.to_ascii_lowercase();
    let condition = condition.trim_start_matches("@media").trim();
    let mut rest = condition;
    // Words outside brackets: media types and the logical keywords.
    let mut outside = String::new();
    while let Some(open) = rest.find('(') {
        outside.push_str(&rest[..open]);
        let Some(close) = rest[open..].find(')') else {
            return false;
        };
        let feature = rest[open + 1..open + close].trim();
        let ok = match feature.split_once(':') {
            Some((name, value)) => match name.trim() {
                "max-width" => BREAKPOINTS.contains(&value.trim()),
                "prefers-reduced-motion" => {
                    matches!(value.trim(), "reduce" | "no-preference")
                }
                _ => false,
            },
            None => feature == "prefers-reduced-motion",
        };
        if !ok {
            return false;
        }
        rest = &rest[open + close + 1..];
    }
    outside.push_str(rest);
    outside
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|word| !word.is_empty())
        .all(|word| matches!(word, "and" | "not" | "only" | "screen" | "print" | "all"))
}

/// The fixed values of docs/styles.md that built-in skins keep: weights 500 and 600 (and the normal 400), the
/// corner radii, 1px borders and 2px accent lines, and body text of 15px with a line height of 1.75.
fn fixed_value_allowed(property: &str, value: &str, selector: &Selector) -> bool {
    let keyword = |value: &str| {
        matches!(
            value,
            "inherit" | "initial" | "unset" | "revert" | "revert-layer"
        ) || value.starts_with("var(")
    };
    if keyword(value) {
        return true;
    }
    match property {
        "font-weight" => matches!(value, "normal" | "400" | "500" | "600"),
        "border-radius"
        | "border-top-left-radius"
        | "border-top-right-radius"
        | "border-bottom-left-radius"
        | "border-bottom-right-radius"
        | "border-start-start-radius"
        | "border-start-end-radius"
        | "border-end-start-radius"
        | "border-end-end-radius" => value.split_whitespace().all(|part| {
            matches!(
                part,
                "0" | "4px" | "6px" | "8px" | "12px" | "9999px" | "50%" | "/"
            ) || part.starts_with("var(")
        }),
        _ if property.starts_with("border") && property.ends_with("width") => value
            .split_whitespace()
            .all(|part| matches!(part, "0" | "1px" | "2px")),
        "outline-width" => matches!(value, "0" | "2px"),
        "font-size" if selector.body_text => value == "15px",
        "line-height" if selector.body_text => value == "1.75",
        _ => true,
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
            _ => Err(reject(parser, Problem::Position)),
        },
        "content" => match single {
            Some(Part::EmptyString) => Ok(()),
            Some(Part::Ident(word)) if matches!(word.as_str(), "none" | "normal") => Ok(()),
            _ => Err(reject(parser, Problem::Content)),
        },
        "z-index" => match single {
            Some(Part::Ident(word)) if word == "auto" => Ok(()),
            Some(Part::Integer(value)) if (-1..=20).contains(value) => Ok(()),
            _ => Err(reject(parser, Problem::ZIndex)),
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

#[cfg(test)]
mod tests {
    use super::*;

    const SKIN_MAX: usize = 256 * 1024;

    fn skin(css: &str, trust: Trust) -> Vec<Finding> {
        check(css, Profile::Skin(trust), SKIN_MAX, 10_000)
    }

    fn codes(findings: &[Finding]) -> Vec<(Severity, String)> {
        findings
            .iter()
            .map(|finding| {
                let code = serde_json::to_value(&finding.problem).unwrap()["code"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                (finding.severity, code)
            })
            .collect()
    }

    #[test]
    fn skins_list_every_problem_and_keep_going() {
        let css = ".a { color: red; background: url(x.png); }\n.b { color: blue !important; }\n@import \"x.css\";\n.c { z-index: 99; }\n.d { color: green; }";
        let findings = skin(css, Trust::External);
        assert_eq!(
            codes(&findings),
            [
                (Severity::Error, "url".to_owned()),
                (Severity::Error, "important".to_owned()),
                (Severity::Error, "at-rule".to_owned()),
                (Severity::Error, "z-index".to_owned()),
            ]
        );
        assert_eq!(
            findings.iter().map(|f| f.line).collect::<Vec<_>>(),
            [Some(1), Some(2), Some(3), Some(4)]
        );
    }

    #[test]
    fn the_built_on_guard_blocks_every_skin() {
        for css in [
            ".colophon__legal { display: none; }",
            "footer .colophon__brand { color: transparent; }",
            "[class*=colophon] { opacity: 0; }",
            "body { opacity: 0; }",
            "html { transform: scale(0); }",
            ":root { filter: blur(20px); }",
            "* { visibility: hidden; }",
            ".site-main ~ footer { max-block-size: 0; overflow: hidden; }",
            ":is(footer) { clip-path: inset(50%); }",
        ] {
            for trust in [Trust::Builtin, Trust::External] {
                let findings = skin(css, trust);
                assert!(
                    findings.iter().any(|f| f.severity == Severity::Error
                        && matches!(f.problem, Problem::Colophon | Problem::GuardedRoot { .. })),
                    "{css}: {findings:?}"
                );
            }
        }
        // Colors and spacing on the page root are fine.
        assert_eq!(
            skin(
                "body { background: var(--bg); color: var(--text); }",
                Trust::External
            ),
            []
        );
        assert_eq!(skin(".footer-note { display: none; }", Trust::External), []);
    }

    #[test]
    fn style_rules_block_built_in_skins_and_warn_for_others() {
        for (css, code) in [
            (
                ".masthead__board { padding-inline: 12px; }",
                "masthead-size",
            ),
            (".forum-more__toggle { font-size: 16px; }", "masthead-size"),
            (".container { padding-inline: 8px; }", "masthead-size"),
            ("a:focus-visible { outline: none; }", "focus-hidden"),
            (".actions-bar > a { opacity: 0; }", "actions-hidden"),
            (
                "@media (max-width: 800px) { .a { color: red; } }",
                "breakpoint",
            ),
            (
                "@media (min-width: 900px) { .a { color: red; } }",
                "breakpoint",
            ),
            (".a { margin-left: 4px; }", "physical"),
            (".a { width: 10px; }", "physical"),
            (".a { font-weight: 700; }", "fixed-value"),
            (".a { border-radius: 3px; }", "fixed-value"),
            (".a { border-block-start-width: 5px; }", "fixed-value"),
            ("body { font-size: 16px; }", "fixed-value"),
            (":root { --r: 2px; }", "design-variable"),
            (":root { --container: 1400px; }", "design-variable"),
            ("[aria-label=\"Reply\"] { color: red; }", "fragile-selector"),
            (
                ".rows__item:nth-child(2) { color: red; }",
                "fragile-selector",
            ),
        ] {
            assert_eq!(
                codes(&skin(css, Trust::Builtin)),
                [(Severity::Error, code.to_owned())],
                "{css}"
            );
            assert_eq!(
                codes(&skin(css, Trust::External)),
                [(Severity::Warning, code.to_owned())],
                "{css}"
            );
        }
    }

    #[test]
    fn styles_that_follow_the_guide_pass_as_built_in() {
        for css in [
            ".panel--aside { border: 0; border-block-start: 1px solid var(--line); border-radius: 0; }",
            ".masthead { background: var(--panel); color: var(--text); }",
            ".masthead__board[aria-current] { color: var(--accent); }",
            "@media (max-width: 760px) { .layout { gap: 16px; } }",
            "@media (prefers-reduced-motion: reduce) { .a { transition: none; } }",
            "@media print { .a { display: none; } }",
            ".a { font-weight: 600; border-radius: var(--r-sm); margin-inline-start: 4px; }",
            ":root { --r: 12px; --gap: 20px; }",
            ".a::before { content: \"\"; }",
            "a:focus-visible { outline-offset: 4px; }",
            ".rows--topics:has(.row-icon) .row-icon { display: inline-flex; }",
            // A universal selector below an ordinary block cannot reach the page frame.
            ".layout > * { opacity: 0.9; }",
        ] {
            assert_eq!(skin(css, Trust::Builtin), [], "{css}");
        }
    }

    #[test]
    fn reserved_variables_and_important_block_every_skin() {
        for trust in [Trust::Builtin, Trust::External] {
            assert_eq!(
                codes(&skin(".a { --gonof-colophon: red; }", trust)),
                [(Severity::Error, "reserved-variable".to_owned())]
            );
            assert_eq!(
                codes(&skin(".a { color: red !important; }", trust)),
                [(Severity::Error, "important".to_owned())]
            );
        }
        // Member styles keep both.
        assert_eq!(
            check(
                ".a { --gonof-x: 1; color: red !important; }",
                Profile::Member,
                16 * 1024,
                1000
            ),
            []
        );
    }

    #[test]
    fn member_styles_stop_at_the_first_error() {
        let findings = check(
            ".a { color: url(x); }\n.b { z-index: 99; }",
            Profile::Member,
            16 * 1024,
            1000,
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].problem, Problem::Url);
        assert_eq!(findings[0].line, Some(1));
    }

    #[test]
    fn findings_are_capped() {
        let css = ".a { z-index: 99; }\n".repeat(MAX_FINDINGS + 5);
        let findings = skin(&css, Trust::External);
        assert_eq!(findings.len(), MAX_FINDINGS + 1);
        assert_eq!(findings.last().unwrap().problem, Problem::More { count: 5 });
    }

    #[test]
    fn findings_round_trip_as_codes() {
        let finding = Finding {
            file: "skin.css".into(),
            line: Some(3),
            severity: Severity::Warning,
            problem: Problem::FixedValue {
                property: "font-weight".into(),
                value: "700".into(),
            },
        };
        let json = serde_json::to_string(&finding).unwrap();
        assert_eq!(
            json,
            r#"{"file":"skin.css","line":3,"severity":"warning","code":"fixed-value","property":"font-weight","value":"700"}"#
        );
        assert_eq!(serde_json::from_str::<Finding>(&json).unwrap(), finding);
    }
}
