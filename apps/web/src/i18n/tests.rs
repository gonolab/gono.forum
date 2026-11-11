//! Translation file checks (docs/translating.md, "Checking a translation") and formatting.
use std::collections::{BTreeMap, BTreeSet};

use fluent_syntax::ast::{Entry, Pattern};

use super::*;

/// Every entry of every file: (language, file, key or `key.attribute`, pattern).
fn entries() -> Vec<(&'static str, &'static str, String, Pattern<&'static str>)> {
    let mut found = Vec::new();
    for (tag, files) in SOURCES {
        for (name, source) in *files {
            let resource = fluent_syntax::parser::parse(*source).unwrap_or_else(|(_, errors)| {
                panic!("locales/{tag}/{name} does not parse: {errors:?}")
            });
            for entry in resource.body {
                match entry {
                    Entry::Message(message) => {
                        let key = message.id.name;
                        if let Some(value) = message.value {
                            found.push((*tag, *name, key.to_owned(), value));
                        }
                        for attribute in message.attributes {
                            found.push((
                                *tag,
                                *name,
                                format!("{key}.{}", attribute.id.name),
                                attribute.value,
                            ));
                        }
                    }
                    Entry::Junk { content } => {
                        panic!("locales/{tag}/{name} has content that does not parse: {content:?}")
                    }
                    _ => {}
                }
            }
        }
    }
    found
}

fn base_variables() -> BTreeMap<String, BTreeSet<&'static str>> {
    entries()
        .into_iter()
        .filter(|(tag, ..)| *tag == "en")
        .map(|(_, _, key, pattern)| (key, syntax::variables(&pattern).into_iter().collect()))
        .collect()
}

#[test]
fn every_translation_parses_and_uses_only_base_keys() {
    let base = base_variables();
    assert_eq!(
        base.keys().cloned().collect::<BTreeSet<_>>(),
        KEYS.iter()
            .map(|key| (*key).to_owned())
            .collect::<BTreeSet<_>>(),
        "the generated key table differs from the en files"
    );
    let mut problems = Vec::new();
    for (tag, name, key, _) in entries() {
        let id = key.split('.').next().unwrap_or_default();
        if !syntax::valid_key(id) {
            problems.push(format!("locales/{tag}/{name}: invalid key name {key}"));
        }
        if !base.contains_key(&key) {
            problems.push(format!("locales/{tag}/{name}: {key} is not in en"));
        }
    }
    // Loading every language panics on duplicated keys across files.
    for lang in Lang::all() {
        assert!(!lang.name().is_empty());
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn translations_use_the_same_variables_as_the_base() {
    let base = base_variables();
    let mut problems = Vec::new();
    for (tag, name, key, pattern) in entries() {
        let used: BTreeSet<&str> = syntax::variables(&pattern).into_iter().collect();
        for variable in &used {
            if !syntax::valid_variable(variable) {
                problems.push(format!(
                    "locales/{tag}/{name}: {key} has an invalid variable name ${variable}"
                ));
            }
        }
        if let Some(expected) = base.get(&key)
            && &used != expected
        {
            problems.push(format!(
                "locales/{tag}/{name}: {key} uses {used:?}, en has {expected:?}"
            ));
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn translations_contain_no_markup_and_no_private_use_characters() {
    let mut problems = Vec::new();
    for (tag, name, key, pattern) in entries() {
        for text in syntax::texts(&pattern) {
            if text.contains('<') {
                problems.push(format!(
                    "locales/{tag}/{name}: {key} contains < (sentences with markup use *_html variables)"
                ));
            }
            if text.chars().any(|c| ('\u{E000}'..='\u{F8FF}').contains(&c)) {
                problems.push(format!(
                    "locales/{tag}/{name}: {key} contains private-use characters"
                ));
            }
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn script_keys_live_in_js_ftl_and_stay_in_the_browser_subset() {
    let mut problems = Vec::new();
    for (tag, name, key, pattern) in entries() {
        let script = key.starts_with("js-");
        if script != (name == "js.ftl") {
            problems.push(format!(
                "locales/{tag}/{name}: {key} (js- keys go only in js.ftl, and js.ftl holds only js- keys)"
            ));
        }
        if key.contains('.') && script {
            problems.push(format!(
                "locales/{tag}/{name}: {key} (script keys have no attributes)"
            ));
        }
        if script && let Err(reason) = syntax::script_shape(&pattern) {
            problems.push(format!("locales/{tag}/{name}: {key}: {reason}"));
        }
        if syntax::variables(&pattern)
            .iter()
            .any(|variable| syntax::is_markup(variable))
            && script
        {
            problems.push(format!(
                "locales/{tag}/{name}: {key} (scripts have no *_html variables)"
            ));
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn every_language_names_itself_and_the_base_is_complete() {
    for (tag, files) in SOURCES {
        let named = files.iter().any(|(name, source)| {
            *name == "meta.ftl"
                && source
                    .lines()
                    .any(|line| line.starts_with("language-name ="))
        });
        assert!(named, "locales/{tag}/meta.ftl has no language-name");
    }
    assert_eq!(Lang::EN.tag(), "en");
    assert_eq!(Lang::EN.percent(), 100);
    assert_eq!(Lang::EN.completion().0, KEYS.len());
    let zh = Lang::parse("zh-CN").expect("zh-CN is built in");
    // The language's own name, "Simplified Chinese".
    assert_eq!(zh.name(), "\u{7b80}\u{4f53}\u{4e2d}\u{6587}");
    assert_eq!(
        Lang::parse("zh"),
        None,
        "tags match the directory name exactly"
    );
    assert_eq!(Lang::all().next(), Some(Lang::EN));
}

#[test]
fn messages_pick_plurals_and_fall_back_to_the_base() {
    let zh = Lang::parse("zh-CN").unwrap();
    assert_eq!(Lang::EN.time_hours_ago(1), "1 hour ago");
    assert_eq!(Lang::EN.time_hours_ago(3), "3 hours ago");
    assert_eq!(Lang::EN.time_days_ago(1), "1 day ago");
    // zh-CN has no plural forms: "3 hours ago" and "just now".
    assert_eq!(zh.time_hours_ago(3), "3 \u{5c0f}\u{65f6}\u{524d}");
    assert_eq!(zh.time_just_now(), "\u{521a}\u{521a}");
    // No bidi isolation marks around placeables.
    assert!(!Lang::EN.brand_level_name(2).contains('\u{2068}'));
    assert_eq!(Lang::EN.brand_level_name(2), "Level 2");
    // A key the language lacks comes from en; an unknown key prints itself.
    assert_eq!(
        zh.format("time-hours-ago", vec![("n", 2.into())]),
        // "2 hours ago".
        "2 \u{5c0f}\u{65f6}\u{524d}"
    );
    assert_eq!(zh.format("no-such-key", Vec::new()), "no-such-key");
}

#[test]
fn numbers_and_dates_follow_the_language() {
    let zh = Lang::parse("zh-CN").unwrap();
    assert_eq!(Lang::EN.number(1_234_567), "1,234,567");
    assert_eq!(Lang::EN.number(-42), "-42");
    assert_eq!(Lang::EN.time_minutes_ago(1500), "1,500 minutes ago");
    // zh-CN writes numbers without grouping (`meta-number-grouping = never`, docs/architecture.md, "Languages").
    assert_eq!(zh.number(1_234_567), "1234567");
    // "1500 minutes ago", without grouping in the message either.
    assert_eq!(zh.time_minutes_ago(1500), "1500 \u{5206}\u{949f}\u{524d}");
    let at = chrono::NaiveDate::from_ymd_opt(2026, 9, 16)
        .unwrap()
        .and_hms_opt(14, 5, 0)
        .unwrap();
    assert_eq!(
        Lang::EN.date(DateField::DateTime, true, at),
        "Sep 16, 2026, 14:05"
    );
    // CLDR puts a narrow no-break space before the day period.
    assert_eq!(
        Lang::EN.date(DateField::DateTime, false, at),
        "Sep 16, 2026, 2:05\u{202f}PM"
    );
    assert_eq!(Lang::EN.date(DateField::Short, true, at), "Sep 16, 14:05");
    assert_eq!(Lang::EN.date(DateField::Date, true, at), "Sep 16, 2026");
    // ICU writes zh-CN dates with the year, month and day signs (U+5E74, U+6708, U+65E5) after the numbers.
    assert_eq!(
        zh.date(DateField::DateTime, true, at),
        "2026\u{5e74}9\u{6708}16\u{65e5} 14:05"
    );
    assert_eq!(
        zh.date(DateField::Short, true, at),
        "9\u{6708}16\u{65e5} 14:05"
    );
    assert_eq!(
        zh.date(DateField::Date, false, at),
        "2026\u{5e74}9\u{6708}16\u{65e5}"
    );
}

#[test]
fn page_attributes_follow_the_language() {
    let zh = Lang::parse("zh-CN").unwrap();
    assert_eq!(Lang::EN.dir(), "");
    assert_eq!(zh.dir(), "");
    assert!(!zh.is_rtl());
    assert_eq!(Lang::EN.og_locale(), "en_US");
    assert_eq!(zh.og_locale(), "zh_CN");
    assert_eq!(serde_json::to_string(&zh).unwrap(), "\"zh-CN\"");
}

#[test]
fn markup_slots_are_inserted_after_escaping() {
    assert_eq!(Markup::text("a<b&\"c'").as_str(), "a&#60;b&#38;&#34;c&#39;");
    let html = Lang::EN.format_markup("brand-level-name", vec![("n", "<x>".into())], vec![]);
    assert_eq!(html.as_str(), "Level &#60;x&#62;");
}

#[test]
fn the_browser_catalog_is_a_script_for_every_language() {
    for lang in Lang::all() {
        let text = script(lang);
        assert!(text.contains("window.gonofT"), "{lang}");
        assert!(
            text.contains(&format!("const lang = \"{}\";", lang.tag())),
            "{lang}"
        );
        assert!(!text.contains('<'), "{lang}");
    }
    // Numbers in browser messages group their digits as the server does: zh-CN writes 1234, English 1,234.
    let zh = Lang::parse("zh-CN").unwrap();
    assert!(script(zh).contains("new Intl.NumberFormat(lang, { useGrouping: false })"));
    assert!(script(Lang::EN).contains("new Intl.NumberFormat(lang, {})"));
}

#[test]
fn rendered_bodies_carry_the_request_language() {
    let spoiler = "<spoiler>\n\nx\n\n</spoiler>";
    let english = in_language(Lang::EN, || {
        crate::markdown::render(spoiler, &std::collections::BTreeMap::new(), None)
    });
    assert!(
        english.contains("<summary>Show spoiler</summary>"),
        "{english}"
    );
    let chinese = in_language(Lang::parse("zh-CN").unwrap(), || {
        crate::markdown::render(spoiler, &std::collections::BTreeMap::new(), None)
    });
    // "Show spoiler" in zh-CN.
    assert!(
        chinese.contains("<summary>\u{67e5}\u{770b}\u{5267}\u{900f}</summary>"),
        "{chinese}"
    );
    // Outside a request: the base language.
    assert_eq!(current(), crate::i18n::default_language());
}
