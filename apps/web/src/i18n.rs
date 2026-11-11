//! Interface languages (docs/architecture.md, "Languages").
//!
//! Translations live in `apps/web/locales/<BCP 47 tag>/*.ftl` and are compiled into the binary by build.rs, which
//! also generates one typed method on [`Lang`] per base (en) message: `frame-more` becomes `Lang::frame_more()`, so
//! a mistyped key or a missing argument fails to compile. Formatting looks the key up in the requested language
//! first and falls back to en; en is complete by construction, so there is always text.
//!
//! The language of a request comes from the site (the site setting, overridden by another domain's own column) and
//! is resolved once at the entry layer together with the rest of the site ([`crate::site::Site::language`]);
//! [`current`] reads it. Background work has no request and passes the site language explicitly.
use std::{fmt, sync::LazyLock};

use fluent_bundle::{FluentArgs, FluentResource, FluentValue, concurrent::FluentBundle};
use fluent_syntax::ast;

mod format;
mod script;
mod syntax;

pub use format::DateField;
pub use script::script;

include!(concat!(env!("OUT_DIR"), "/locales.rs"));

/// The product name, shown in every language when the site name or the header brand text is left empty
/// Not translated.
pub const PRODUCT_NAME: &str = "Gono Forum";

/// One language compiled into the binary.
struct Language {
    tag: &'static str,
    bundle: FluentBundle<FluentResource>,
    /// The language's own name (`language-name`, e.g. "Deutsch").
    name: String,
    rtl: bool,
    /// `og:locale`: language and region after ICU likely-subtag expansion (`en` → `en_US`).
    og_locale: String,
    /// Base keys (messages and attributes) this language translates.
    translated: usize,
    dates: format::Dates,
}

impl Language {
    fn load(tag: &'static str, files: &[(&'static str, &'static str)]) -> Self {
        let id: unic_langid::LanguageIdentifier = tag
            .parse()
            .unwrap_or_else(|error| panic!("locales/{tag} is not a valid language tag: {error}"));
        let locale: icu_locale_core::Locale = tag
            .parse()
            .unwrap_or_else(|error| panic!("locales/{tag} is not a valid ICU locale: {error}"));
        let mut bundle = FluentBundle::new_concurrent(vec![id]);
        // The page sets `dir`; Unicode isolation marks around placeables would end up as stray characters in HTML.
        bundle.set_use_isolating(false);
        bundle.set_formatter(Some(format::number_value));
        for (name, source) in files {
            let resource =
                FluentResource::try_new((*source).to_owned()).unwrap_or_else(|(_, errors)| {
                    panic!("locales/{tag}/{name} does not parse: {errors:?}")
                });
            bundle.add_resource(resource).unwrap_or_else(|errors| {
                panic!("locales/{tag}/{name} repeats keys of another file: {errors:?}")
            });
        }
        let translated = KEYS
            .iter()
            .filter(|key| pattern(&bundle, key).is_some())
            .count();
        let name = pattern(&bundle, "language-name")
            .map(|pattern| {
                bundle
                    .format_pattern(pattern, None, &mut Vec::new())
                    .into_owned()
            })
            .unwrap_or_else(|| tag.to_owned());
        // `meta-number-grouping`: `never` turns digit grouping off; anything else (or a missing key) keeps the
        // language's own grouping (docs/architecture.md, "Languages").
        let grouping = pattern(&bundle, "meta-number-grouping").is_none_or(|pattern| {
            bundle.format_pattern(pattern, None, &mut Vec::new()).trim() != "never"
        });
        let mut expanded = locale.clone();
        icu_locale::LocaleExpander::new_common().maximize(&mut expanded.id);
        let og_locale = match expanded.id.region {
            Some(region) => format!("{}_{}", expanded.id.language, region),
            None => expanded.id.language.to_string(),
        };
        Self {
            tag,
            rtl: icu_locale::LocaleDirectionality::new_common().is_right_to_left(&locale.id),
            dates: format::Dates::new(locale, grouping),
            bundle,
            name,
            og_locale,
            translated,
        }
    }
}

/// The pattern of `key` or `key.attribute` in one bundle.
fn pattern<'b>(
    bundle: &'b FluentBundle<FluentResource>,
    key: &str,
) -> Option<&'b ast::Pattern<&'b str>> {
    let message = match key.split_once('.') {
        Some((id, attribute)) => {
            return bundle
                .get_message(id)?
                .get_attribute(attribute)
                .map(|a| a.value());
        }
        None => bundle.get_message(key)?,
    };
    message.value()
}

static LANGUAGES: LazyLock<Vec<Language>> = LazyLock::new(|| {
    SOURCES
        .iter()
        .map(|(tag, files)| Language::load(tag, files))
        .collect()
});

/// A language compiled into the binary. `Copy`, cheap to pass into templates (`p.t`) and to put into cache keys
/// (serialized as its tag).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Lang(u8);

impl Default for Lang {
    fn default() -> Self {
        Self::EN
    }
}

impl serde::Serialize for Lang {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.tag())
    }
}

impl fmt::Display for Lang {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.tag())
    }
}

impl Lang {
    /// The base language (build.rs puts it first).
    pub const EN: Self = Self(0);

    /// The language with exactly this tag (`en`, `zh-CN`); `None` when it is not compiled in.
    pub fn parse(tag: &str) -> Option<Self> {
        SOURCES
            .iter()
            .position(|(candidate, _)| *candidate == tag)
            .and_then(|index| u8::try_from(index).ok())
            .map(Self)
    }

    /// Every compiled-in language, the base first, then by tag.
    pub fn all() -> impl Iterator<Item = Self> {
        (0..SOURCES.len()).filter_map(|index| u8::try_from(index).ok().map(Self))
    }

    fn language(self) -> &'static Language {
        &LANGUAGES[usize::from(self.0)]
    }

    pub fn tag(self) -> &'static str {
        SOURCES[usize::from(self.0)].0
    }

    /// The language's own name, as its translators wrote it.
    pub fn name(self) -> &'static str {
        &self.language().name
    }

    pub fn is_rtl(self) -> bool {
        self.language().rtl
    }

    /// The value of `<html dir>`: `rtl` for right-to-left languages, empty otherwise (`ltr` is the default, so pages
    /// in left-to-right languages do not carry the attribute, docs/architecture.md, "Languages").
    pub fn dir(self) -> &'static str {
        if self.is_rtl() { "rtl" } else { "" }
    }

    pub fn og_locale(self) -> &'static str {
        &self.language().og_locale
    }

    /// (translated, total) base keys.
    pub fn completion(self) -> (usize, usize) {
        (self.language().translated, KEYS.len())
    }

    /// Completion in whole percent, rounded down so that 100 means complete.
    pub fn percent(self) -> usize {
        let (translated, total) = self.completion();
        (translated * 100).checked_div(total).unwrap_or(100)
    }

    /// An integer written the way this language writes numbers (ICU4X, `1,234`).
    pub fn number(self, value: i64) -> String {
        format::number(&self.language().dates, value)
    }

    /// A date or time in this language's style (date style `locale`, docs/architecture.md, "Languages").
    pub fn date(self, field: DateField, clock24: bool, at: chrono::NaiveDateTime) -> String {
        self.language().dates.format(field, clock24, at)
    }

    /// A message without variables as a `&'static str`: labels and error types keep their `&'static str` types, so
    /// each language formats such a message once and keeps it for the life of the process (bounded by the number of
    /// keys times the number of languages).
    fn fixed(self, index: usize) -> &'static str {
        static FIXED: LazyLock<Vec<std::sync::OnceLock<&'static str>>> = LazyLock::new(|| {
            (0..KEYS.len() * SOURCES.len())
                .map(|_| std::sync::OnceLock::new())
                .collect()
        });
        FIXED[usize::from(self.0) * KEYS.len() + index]
            .get_or_init(|| Box::leak(self.format(KEYS[index], Vec::new()).into_boxed_str()))
    }

    fn format(self, key: &'static str, args: Vec<(&'static str, Arg)>) -> String {
        let arguments = (!args.is_empty()).then(|| {
            let mut arguments = FluentArgs::with_capacity(args.len());
            for (name, Arg(value)) in args {
                arguments.set(name, value);
            }
            arguments
        });
        for language in [self.language(), Self::EN.language()] {
            if let Some(found) = pattern(&language.bundle, key) {
                let mut errors = Vec::new();
                let text = language
                    .bundle
                    .format_pattern(found, arguments.as_ref(), &mut errors);
                if !errors.is_empty() {
                    tracing::warn!(
                        key,
                        language = language.tag,
                        ?errors,
                        "translation formatted with errors"
                    );
                }
                return text.into_owned();
            }
        }
        key.to_owned()
    }

    /// Formats a message with markup slots (`*_html` variables): the slots go in as private-use sentinels, the whole
    /// text is HTML-escaped like any template output, and then the sentinels are replaced by the caller's markup.
    /// Translations can contain neither `<` nor private-use characters (checked), so nothing else can turn into
    /// markup.
    fn format_markup(
        self,
        key: &'static str,
        mut args: Vec<(&'static str, Arg)>,
        slots: Vec<(&'static str, Markup)>,
    ) -> Markup {
        for (index, (name, _)) in slots.iter().enumerate() {
            args.push((name, Arg::from(sentinel(index))));
        }
        let text = self.format(key, args);
        let mut html = askama::filters::escape(&text, askama::filters::Html)
            .map(|escaped| escaped.to_string())
            .unwrap_or_default();
        for (index, (_, Markup(markup))) in slots.iter().enumerate() {
            html = html.replace(&sentinel(index), markup);
        }
        Markup(html)
    }
}

fn sentinel(index: usize) -> String {
    format!("\u{E000}{index}\u{E001}")
}

/// The language of this request; outside a request, the process default ([`default_language`]).
pub fn current() -> Lang {
    crate::site::language()
}

/// The language [`current`] answers outside a request: always the base language. Background work passes the site
/// language explicitly instead of relying on it, and tests that need another language outside a request run inside
/// [`crate::site::scope`] or [`in_language`].
pub fn default_language() -> Lang {
    Lang::EN
}

/// Runs synchronous code as if the request were in `lang` (unit tests that render outside a request).
pub fn in_language<R>(lang: Lang, f: impl FnOnce() -> R) -> R {
    crate::site::sync_scope(
        crate::site::Site {
            language: lang,
            ..crate::site::Site::default()
        },
        f,
    )
}

/// An argument of a generated method: numbers stay numbers (plural selection, ICU formatting), everything else is
/// text.
#[derive(Clone, Debug)]
pub struct Arg(FluentValue<'static>);

/// Templates pass arguments by reference (askama), Rust code mostly by value; both convert.
macro_rules! number_args {
    ($($type:ty),*) => {$(
        impl From<$type> for Arg {
            fn from(value: $type) -> Self {
                Self(FluentValue::from(value))
            }
        }
        impl From<&$type> for Arg {
            fn from(value: &$type) -> Self {
                Self(FluentValue::from(*value))
            }
        }
        // Templates bind `if let Some(x)` values by reference, so numbers sometimes arrive twice borrowed.
        impl From<&&$type> for Arg {
            fn from(value: &&$type) -> Self {
                Self(FluentValue::from(**value))
            }
        }
    )*};
}
number_args!(i8, i16, i32, i64, u8, u16, u32, u64, usize, isize);

impl From<&str> for Arg {
    fn from(value: &str) -> Self {
        Self(FluentValue::from(value.to_owned()))
    }
}

impl From<String> for Arg {
    fn from(value: String) -> Self {
        Self(FluentValue::from(value))
    }
}

impl From<&String> for Arg {
    fn from(value: &String) -> Self {
        Self(FluentValue::from(value.clone()))
    }
}

impl From<&&str> for Arg {
    fn from(value: &&str) -> Self {
        Self::from(*value)
    }
}

impl From<&&String> for Arg {
    fn from(value: &&String) -> Self {
        Self::from(*value)
    }
}

/// Already escaped HTML: the only way to put markup into a translated sentence (docs/architecture.md, "Languages").
/// Built by code, never from a translation. Templates print it with `|safe`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Markup(String);

impl Markup {
    /// Markup the caller has built and escaped itself.
    pub fn trusted(html: impl Into<String>) -> Self {
        Self(html.into())
    }

    /// Plain text escaped as a template would.
    pub fn text(text: &str) -> Self {
        Self(
            askama::filters::escape(text, askama::filters::Html)
                .map(|escaped| escaped.to_string())
                .unwrap_or_default(),
        )
    }

    /// `text` escaped and wrapped in one element without attributes, such as `<code>…</code>` or `<strong>…</strong>`,
    /// for a slot of a translation. Templates call it as `crate::i18n::Markup::tag("code", value)`: askama resolves
    /// template variables in function arguments, but not inside a `format!` written in a template.
    pub fn tag(name: &str, text: impl fmt::Display) -> Self {
        Self(format!(
            "<{name}>{}</{name}>",
            Self::text(&text.to_string())
        ))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Markup {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests;
