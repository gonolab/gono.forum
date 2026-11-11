//! Walks parsed Fluent patterns. Shared by build.rs (which generates one method per base message) and the translation
//! checks, so both see the same variables in the same order (docs/architecture.md, "Languages";
//! docs/translating.md, "Checking a translation").
#![allow(dead_code)]

use fluent_syntax::ast::{Expression, InlineExpression, Pattern, PatternElement};

/// Variables a message uses (value or one attribute), in order of first appearance, each once. Selectors count:
/// `{ $n -> … }` uses `n` even if no variant prints it.
pub fn variables<'s>(pattern: &Pattern<&'s str>) -> Vec<&'s str> {
    let mut found = Vec::new();
    walk_pattern(pattern, &mut found);
    found
}

fn walk_pattern<'s>(pattern: &Pattern<&'s str>, found: &mut Vec<&'s str>) {
    for element in &pattern.elements {
        if let PatternElement::Placeable { expression } = element {
            walk_expression(expression, found);
        }
    }
}

fn walk_expression<'s>(expression: &Expression<&'s str>, found: &mut Vec<&'s str>) {
    match expression {
        Expression::Inline(inline) => walk_inline(inline, found),
        Expression::Select { selector, variants } => {
            walk_inline(selector, found);
            for variant in variants {
                walk_pattern(&variant.value, found);
            }
        }
    }
}

fn walk_inline<'s>(inline: &InlineExpression<&'s str>, found: &mut Vec<&'s str>) {
    match inline {
        InlineExpression::VariableReference { id } => {
            if !found.contains(&id.name) {
                found.push(id.name);
            }
        }
        InlineExpression::FunctionReference { arguments, .. } => {
            for argument in &arguments.positional {
                walk_inline(argument, found);
            }
            for argument in &arguments.named {
                walk_inline(&argument.value, found);
            }
        }
        InlineExpression::TermReference {
            arguments: Some(arguments),
            ..
        } => {
            for argument in &arguments.positional {
                walk_inline(argument, found);
            }
            for argument in &arguments.named {
                walk_inline(&argument.value, found);
            }
        }
        InlineExpression::Placeable { expression } => walk_expression(expression, found),
        _ => {}
    }
}

/// Every piece of literal text a pattern can print (text elements and string literals), for the checks that forbid
/// markup and private-use characters in translations.
pub fn texts<'s>(pattern: &Pattern<&'s str>) -> Vec<&'s str> {
    let mut found = Vec::new();
    texts_in_pattern(pattern, &mut found);
    found
}

fn texts_in_pattern<'s>(pattern: &Pattern<&'s str>, found: &mut Vec<&'s str>) {
    for element in &pattern.elements {
        match element {
            PatternElement::TextElement { value } => found.push(value),
            PatternElement::Placeable { expression } => texts_in_expression(expression, found),
        }
    }
}

fn texts_in_expression<'s>(expression: &Expression<&'s str>, found: &mut Vec<&'s str>) {
    match expression {
        Expression::Inline(inline) => texts_in_inline(inline, found),
        Expression::Select { selector, variants } => {
            texts_in_inline(selector, found);
            for variant in variants {
                texts_in_pattern(&variant.value, found);
            }
        }
    }
}

fn texts_in_inline<'s>(inline: &InlineExpression<&'s str>, found: &mut Vec<&'s str>) {
    match inline {
        InlineExpression::StringLiteral { value } => found.push(value),
        InlineExpression::Placeable { expression } => texts_in_expression(expression, found),
        _ => {}
    }
}

/// The Rust method name of a message key or `key.attribute`: hyphens and dots become underscores
/// (`frame-search.placeholder` → `frame_search_placeholder`).
pub fn method_name(key: &str) -> String {
    key.replace(['-', '.'], "_")
}

/// The Rust parameter name of a Fluent variable. Variables use lowercase letters, digits and underscores (checked);
/// a Rust keyword gets a trailing underscore.
pub fn parameter_name(variable: &str) -> String {
    const KEYWORDS: &[&str] = &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
        "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move",
        "mut", "pub", "ref", "return", "self", "static", "struct", "super", "trait", "true",
        "type", "unsafe", "use", "where", "while", "yield",
    ];
    if KEYWORDS.contains(&variable) {
        format!("{variable}_")
    } else {
        variable.to_owned()
    }
}

/// Message keys: lowercase letters, digits and single hyphens, starting with a letter
/// (docs/architecture.md, "Languages").
pub fn valid_key(key: &str) -> bool {
    key.starts_with(|c: char| c.is_ascii_lowercase())
        && !key.ends_with('-')
        && !key.contains("--")
        && key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Variable names: lowercase letters, digits and underscores, starting with a letter. Names ending in `_html` are
/// markup slots (docs/architecture.md, "Languages").
pub fn valid_variable(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// A markup slot: the caller passes already escaped HTML for it.
pub fn is_markup(name: &str) -> bool {
    name.ends_with("_html")
}

/// Whether a `js.ftl` message stays inside the subset the browser formatter understands
/// (docs/architecture.md, "Languages"): plain text and `{ $var }`, or the whole message being one select on a variable
/// whose variants are again only those two.
pub fn script_shape(pattern: &Pattern<&str>) -> Result<(), String> {
    fn flat(pattern: &Pattern<&str>) -> bool {
        pattern.elements.iter().all(|element| {
            matches!(
                element,
                PatternElement::TextElement { .. }
                    | PatternElement::Placeable {
                        expression: Expression::Inline(InlineExpression::VariableReference { .. }),
                    }
            )
        })
    }
    if flat(pattern) {
        return Ok(());
    }
    match pattern.elements.as_slice() {
        [
            PatternElement::Placeable {
                expression:
                    Expression::Select {
                        selector: InlineExpression::VariableReference { .. },
                        variants,
                    },
            },
        ] if variants.iter().all(|variant| flat(&variant.value)) => Ok(()),
        _ => Err(
            "js.ftl only allows text, { $var }, or one select on a variable over such variants"
                .into(),
        ),
    }
}
