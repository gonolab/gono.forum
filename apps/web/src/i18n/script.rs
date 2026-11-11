//! The browser's share of the translations (docs/architecture.md, "Languages"): the `js-*` keys of one language,
//! missing ones taken from en, as a small script served at `/static/i18n/<tag>.js`. It defines
//! `window.gonofT(key, args)`, which fills `{ $var }` (whole numbers through `Intl.NumberFormat`, like the server's
//! ICU formatting, grouping digits only when the language does: `meta-number-grouping`) and picks plural variants
//! with `Intl.PluralRules`.
//!
//! `js.ftl` stays inside a subset the browser can format without a Fluent runtime (checked by
//! [`super::syntax::script_shape`]): text and `{ $var }`, or one select on a variable whose variants are again only
//! those two.
use std::{collections::BTreeMap, sync::LazyLock};

use fluent_syntax::ast::{Expression, InlineExpression, Pattern, PatternElement, VariantKey};
use serde_json::{Value, json};

use super::{KEYS, LANGUAGES, Lang, pattern};

/// Prefix of the keys the browser receives.
const PREFIX: &str = "js-";

/// The finished scripts, one per language, built once.
static SCRIPTS: LazyLock<Vec<String>> = LazyLock::new(|| Lang::all().map(build).collect());

/// The script for one language.
pub fn script(lang: Lang) -> &'static str {
    &SCRIPTS[usize::from(lang.0)]
}

fn build(lang: Lang) -> String {
    let mut messages = BTreeMap::new();
    for key in KEYS
        .iter()
        .filter(|key| key.starts_with(PREFIX) && !key.contains('.'))
    {
        for language in [&LANGUAGES[usize::from(lang.0)], &LANGUAGES[0]] {
            if let Some(found) = pattern(&language.bundle, key) {
                messages.insert(*key, compile(found));
                break;
            }
        }
    }
    let catalog = serde_json::to_string(&messages).unwrap_or_else(|_| "{}".into());
    let quoted = serde_json::to_string(lang.tag()).unwrap_or_else(|_| "\"en\"".into());
    let raw = lang.tag();
    let grouping = if LANGUAGES[usize::from(lang.0)].dates.grouping() {
        "{}"
    } else {
        "{ useGrouping: false }"
    };
    format!(
        "// Generated from apps/web/locales/{raw}/js.ftl.\n\
         (() => {{\n\
         \x20 const lang = {quoted};\n\
         \x20 const messages = {catalog};\n\
         \x20 let rules, numbers;\n\
         \x20 const show = (value) => (typeof value === 'number' && Number.isInteger(value) ? (numbers ||= new Intl.NumberFormat(lang, {grouping})).format(value) : String(value));\n\
         \x20 const fill = (text, args) => text.replace(/\\{{\\$([a-z0-9_]+)\\}}/g, (_, name) => (args && name in args ? show(args[name]) : ''));\n\
         \x20 window.gonofT = (key, args) => {{\n\
         \x20   const message = messages[key];\n\
         \x20   if (message === undefined) return key;\n\
         \x20   if (typeof message === 'string') return fill(message, args);\n\
         \x20   const value = args ? args[message.on] : undefined;\n\
         \x20   let variant = message.cases[String(value)];\n\
         \x20   if (variant === undefined && typeof value === 'number') {{\n\
         \x20     rules ||= new Intl.PluralRules(lang);\n\
         \x20     variant = message.cases[rules.select(value)];\n\
         \x20   }}\n\
         \x20   return fill(variant === undefined ? message.cases[message.other] : variant, args);\n\
         \x20 }};\n\
         }})();\n"
    )
}

/// A flat pattern as a string with `{$var}` placeholders, or a select as `{on, cases, other}`.
fn compile(pattern: &Pattern<&str>) -> Value {
    if let [
        PatternElement::Placeable {
            expression:
                Expression::Select {
                    selector: InlineExpression::VariableReference { id },
                    variants,
                },
        },
    ] = pattern.elements.as_slice()
    {
        let mut cases = serde_json::Map::new();
        let mut other = String::new();
        for variant in variants {
            let key = match &variant.key {
                VariantKey::Identifier { name } => (*name).to_owned(),
                VariantKey::NumberLiteral { value } => (*value).to_owned(),
            };
            if variant.default {
                other.clone_from(&key);
            }
            cases.insert(key, Value::String(flat(&variant.value)));
        }
        return json!({"on": id.name, "cases": cases, "other": other});
    }
    Value::String(flat(pattern))
}

fn flat(pattern: &Pattern<&str>) -> String {
    let mut text = String::new();
    for element in &pattern.elements {
        match element {
            PatternElement::TextElement { value } => text.push_str(value),
            PatternElement::Placeable {
                expression: Expression::Inline(InlineExpression::VariableReference { id }),
            } => {
                text.push_str("{$");
                text.push_str(id.name);
                text.push('}');
            }
            // Outside the subset; the translation checks reject it before it can ship.
            PatternElement::Placeable { .. } => {}
        }
    }
    text
}
