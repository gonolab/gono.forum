//! For display on the debug page only; the executor always runs the original SQL with the original bound parameters.
//! Literals are written for the backend that executed the statement: the SQL in the debug record is the text actually
//! executed — on SQLite it has already been through the rewriter — and once expanded it must replay verbatim on the
//! same backend.
use super::Backend;

#[derive(Clone, Debug, PartialEq)]
pub enum SqlValue {
    Null,
    Bool(bool),
    Integer(i64),
    Unsigned(u64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
}

/// Expands parameters by SQL token (MySQL syntax), without scanning generated literals and never truncating
/// parameters.
/// Returns an error when the parameter mapping cannot be determined; callers should keep the original SQL and
/// parameters for inspection.
pub fn expand_sql(sql: &str, values: &[SqlValue]) -> Result<String, String> {
    expand_sql_for(Backend::MySql, sql, values)
}

/// Same as [`expand_sql`], with literals and lexing written for `backend`: in SQLite strings a backslash has no escape
/// meaning, and there are no `#` comments.
pub fn expand_sql_for(backend: Backend, sql: &str, values: &[SqlValue]) -> Result<String, String> {
    let literals = values
        .iter()
        .map(|value| literal_for(backend, value))
        .collect::<Result<Vec<_>, _>>()?;
    if backend == Backend::Sqlite {
        return expand(sql, &literals, false, false);
    }
    let result = expand(sql, &literals, false, true);
    if sql.contains('\\') {
        // The original SQL does not carry the session sql_mode. When the two legal modes interpret the placeholder
        // positions differently, refuse to fabricate a complete SQL; the generated text literals themselves do not
        // depend on the mode.
        let escaped = expand(sql, &literals, true, true);
        if result != escaped {
            return Err(crate::i18n::current().debug_expand_backslash().into());
        }
    }
    result
}

#[cfg(test)]
fn literal(value: &SqlValue) -> Result<String, String> {
    literal_for(Backend::MySql, value)
}

fn literal_for(backend: Backend, value: &SqlValue) -> Result<String, String> {
    if let (Backend::Sqlite, SqlValue::Text(value)) = (backend, value) {
        // In SQLite strings a backslash is just the character itself; control characters go through byte literals as on
        // MySQL, so they are not affected by HTML newline normalization after copying.
        return Ok(if value.chars().any(char::is_control) {
            format!("CAST(X'{}' AS TEXT)", hex(value.as_bytes()))
        } else {
            format!("'{}'", value.replace('\'', "''"))
        });
    }
    Ok(match value {
        SqlValue::Null => "NULL".into(),
        SqlValue::Bool(value) => if *value { "TRUE" } else { "FALSE" }.into(),
        SqlValue::Integer(value) => value.to_string(),
        SqlValue::Unsigned(value) => value.to_string(),
        SqlValue::Float(value) => {
            if !value.is_finite() {
                return Err(crate::i18n::current().debug_expand_float().into());
            }
            let mut number = value.to_string();
            if !number.contains(['.', 'e', 'E']) {
                number.push_str(".0");
            }
            if !number.contains(['e', 'E']) {
                number.push_str("e0");
            }
            number
        }
        SqlValue::Text(value) => {
            // HTML normalizes raw CR/CRLF. Byte literals keep control characters exact after copying,
            // and also keep text backslashes independent of the session sql_mode.
            if value.chars().any(|ch| ch == '\\' || ch.is_control()) {
                format!("CONVERT(X'{}' USING utf8mb4)", hex(value.as_bytes()))
            } else {
                format!("'{}'", value.replace('\'', "''"))
            }
        }
        SqlValue::Bytes(value) => format!("X'{}'", hex(value)),
    })
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8] = b"0123456789ABCDEF";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 15) as usize] as char);
    }
    output
}

fn expand(
    sql: &str,
    literals: &[String],
    mysql_backslash_escapes: bool,
    mysql: bool,
) -> Result<String, String> {
    let bytes = sql.as_bytes();
    let mut output = String::with_capacity(sql.len());
    let mut used = vec![false; literals.len()];
    let mut highest_parameter = 0;
    let mut position = 0;
    while position < bytes.len() {
        let start = position;
        let current = bytes[position];
        if current == b'\'' || current == b'"' || current == b'`' {
            let backslash = mysql_backslash_escapes && current != b'`';
            position = quoted(bytes, position, current, backslash)?;
        } else if (bytes[position..].starts_with(b"--")
            && bytes
                .get(position + 2)
                .is_none_or(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control()))
            || (mysql && current == b'#')
        {
            position = bytes[position..]
                .iter()
                .position(|byte| *byte == b'\n' || *byte == b'\r')
                .map_or(bytes.len(), |end| position + end);
        } else if bytes[position..].starts_with(b"/*") {
            if mysql
                && (bytes[position..].starts_with(b"/*!") || bytes[position..].starts_with(b"/*M!"))
            {
                return Err(crate::i18n::current()
                    .debug_expand_executable_comment()
                    .into());
            }
            position = block_comment(bytes, position)?;
        } else if current == b'?' {
            if bytes.get(position + 1).is_some_and(u8::is_ascii_digit) {
                return Err(crate::i18n::current().debug_expand_numbered().into());
            }
            highest_parameter += 1;
            append_parameter(&mut output, literals, &mut used, highest_parameter)?;
            position += 1;
            continue;
        } else if identifier_start(current) {
            position += 1;
            while bytes.get(position).is_some_and(|b| identifier_part(*b)) {
                position += 1;
            }
        } else {
            position += 1;
        }
        // Non-ASCII bytes only appear in the identifier tokens above or inside quotes/comments, so slices always fall
        // on UTF-8 boundaries.
        output.push_str(&sql[start..position]);
    }
    if let Some(index) = used.iter().position(|used| !used) {
        return Err(crate::i18n::current().debug_expand_unused(index + 1));
    }
    Ok(output)
}

fn append_parameter(
    output: &mut String,
    literals: &[String],
    used: &mut [bool],
    index: usize,
) -> Result<(), String> {
    let offset = index
        .checked_sub(1)
        .ok_or(crate::i18n::current().debug_expand_from_one())?;
    let literal = literals
        .get(offset)
        .ok_or_else(|| crate::i18n::current().debug_expand_missing(index))?;
    // For example `1 - ?` bound to -1; even if the original SQL is written `1-?`, it must not become the line comment
    // `1--1`.
    if literal.starts_with('-') {
        output.push('(');
        output.push_str(literal);
        output.push(')');
    } else {
        output.push_str(literal);
    }
    used[offset] = true;
    Ok(())
}

fn identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || !byte.is_ascii()
}

fn identifier_part(byte: u8) -> bool {
    identifier_start(byte) || byte.is_ascii_digit() || byte == b'$'
}

fn quoted(bytes: &[u8], start: usize, quote: u8, backslash: bool) -> Result<usize, String> {
    let mut position = start + 1;
    while position < bytes.len() {
        if bytes[position] == b'\\' && backslash {
            position += 2;
        } else if bytes[position] == quote {
            if bytes.get(position + 1) == Some(&quote) {
                position += 2;
            } else {
                return Ok(position + 1);
            }
        } else {
            position += 1;
        }
    }
    Err(crate::i18n::current().debug_expand_unclosed_string().into())
}

fn block_comment(bytes: &[u8], start: usize) -> Result<usize, String> {
    let mut position = start + 2;
    while position < bytes.len() {
        if bytes[position..].starts_with(b"*/") {
            return Ok(position + 2);
        }
        position += 1;
    }
    Err(crate::i18n::current()
        .debug_expand_unclosed_comment()
        .into())
}

#[cfg(test)]
mod tests {
    use super::{Backend, SqlValue, expand_sql, expand_sql_for, literal};

    #[test]
    fn sqlite_literals_keep_backslashes_and_encode_controls_as_text_blobs() {
        assert_eq!(
            expand_sql_for(
                Backend::Sqlite,
                "SELECT ?, ?, ? # not a comment",
                &[
                    SqlValue::Text(r"a\b 'c'".into()),
                    // "\u{884c}" is the Chinese for "line": three bytes in UTF-8 (E8A18C).
                    SqlValue::Text("\u{884c}\r\n".into()),
                    SqlValue::Bytes(vec![0, 255]),
                ]
            )
            .unwrap(),
            r"SELECT 'a\b ''c''', CAST(X'E8A18C0D0A' AS TEXT), X'00FF' # not a comment"
        );
        // In MySQL a leading `#` starts a comment, and question marks inside it are not parameters.
        assert!(expand_sql("SELECT ? # ?", &[SqlValue::Integer(1)]).is_ok());
    }

    #[test]
    fn preserves_unicode_quotes_and_sql_looking_parameters() {
        // "\u{91d1}\u{5e01}" is the Chinese for "coins": multi-byte characters must pass through unchanged.
        let value = SqlValue::Text("\u{91d1}\u{5e01}🍓 'quoted' ? $1 $10 -- /* */ </code>".into());
        assert_eq!(
            expand_sql("SELECT ?, ?", &[value, SqlValue::Integer(12)]).unwrap(),
            "SELECT '\u{91d1}\u{5e01}🍓 ''quoted'' ? $1 $10 -- /* */ </code>', 12"
        );
    }

    #[test]
    fn skips_quotes_identifiers_and_comments() {
        assert_eq!(
            expand_sql(
                "SELECT '?', 'it''s ?', \"?\", `?`, ? -- ?\n/* ? */ , ?",
                &[SqlValue::Integer(8), SqlValue::Integer(9)],
            )
            .unwrap(),
            "SELECT '?', 'it''s ?', \"?\", `?`, 8 -- ?\n/* ? */ , 9"
        );
    }

    #[test]
    fn respects_comment_rules_and_rejects_mode_ambiguity() {
        let sql = "SELECT 'a\\b ?', `a\\`, ? # ?\r\n, ? -- ?\n, 1--?";
        assert_eq!(
            expand_sql(
                sql,
                &[
                    SqlValue::Integer(1),
                    SqlValue::Integer(2),
                    SqlValue::Integer(3),
                ],
            )
            .unwrap(),
            "SELECT 'a\\b ?', `a\\`, 1 # ?\r\n, 2 -- ?\n, 1--3"
        );
        assert!(expand_sql(r"SELECT 'a\'?', ?", &[SqlValue::Null]).is_err());
        for sql in ["SELECT /*! ? */ ?", "SELECT /*M! ? */ ?"] {
            assert!(expand_sql(sql, &[SqlValue::Null]).is_err());
        }
    }

    #[test]
    fn encodes_text_with_controls_without_mode_dependent_escapes() {
        // Ends with the Chinese for "coins" ("\u{91d1}\u{5e01}", E98791 E5B881) and an emoji (F09F8D93).
        let value = SqlValue::Text("a'\\\0\n\r\t\x08\x0c\x011\u{91d1}\u{5e01}🍓".into());
        assert_eq!(
            literal(&value).unwrap(),
            "CONVERT(X'61275C000A0D09080C0131E98791E5B881F09F8D93' USING utf8mb4)"
        );
        assert_eq!(
            literal(&SqlValue::Text(r"a\b".into())).unwrap(),
            "CONVERT(X'615C62' USING utf8mb4)"
        );
    }

    #[test]
    fn control_values_survive_html_newline_normalization() {
        for (text, hex) in [
            ("a\rb", "610D62"),
            ("a\r\nb", "610D0A62"),
            ("a\nb", "610A62"),
            ("a\tb", "610962"),
            ("a\x7fb", "617F62"),
        ] {
            let expanded = expand_sql("SELECT ?", &[SqlValue::Text(text.into())]).unwrap();
            assert_eq!(expanded, format!("SELECT CONVERT(X'{hex}' USING utf8mb4)"));
            assert!(!expanded.chars().any(char::is_control));
        }
    }

    #[test]
    fn renders_binary_and_numeric_values_without_loss() {
        assert_eq!(literal(&SqlValue::Null).unwrap(), "NULL");
        assert_eq!(
            literal(&SqlValue::Integer(i64::MIN)).unwrap(),
            "-9223372036854775808"
        );
        assert_eq!(
            literal(&SqlValue::Unsigned(u64::MAX)).unwrap(),
            "18446744073709551615"
        );
        for (value, expected) in [(-0.0, "-0.0e0"), (1.5, "1.5e0"), (2.0, "2.0e0")] {
            assert_eq!(literal(&SqlValue::Float(value)).unwrap(), expected);
        }
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(literal(&SqlValue::Float(value)).is_err());
        }
        assert_eq!(
            literal(&SqlValue::Bytes(vec![0, 39, 255])).unwrap(),
            "X'0027FF'"
        );
        assert_eq!(literal(&SqlValue::Bool(true)).unwrap(), "TRUE");
        assert_eq!(literal(&SqlValue::Bool(false)).unwrap(), "FALSE");
    }

    #[test]
    fn negative_parameters_cannot_turn_adjacent_minus_into_comments() {
        assert_eq!(
            expand_sql(
                "SELECT 1-?, -?",
                &[SqlValue::Integer(-1), SqlValue::Integer(-2)],
            )
            .unwrap(),
            "SELECT 1-(-1), -(-2)"
        );
    }

    #[test]
    fn rejects_missing_unused_invalid_and_unclosed_parameters() {
        assert!(expand_sql("SELECT ?", &[]).is_err());
        assert!(expand_sql("SELECT 1", &[SqlValue::Null]).is_err());
        for sql in [
            "SELECT 'unterminated",
            "SELECT /* unterminated",
            "SELECT `unterminated",
        ] {
            assert!(expand_sql(sql, &[]).is_err(), "{sql}");
        }
        for sql in ["SELECT ?0", "SELECT ?1", "SELECT ?999999999999999999999999"] {
            assert!(expand_sql(sql, &[SqlValue::Null]).is_err(), "{sql}");
        }
    }

    #[test]
    fn never_truncates_long_values_or_many_parameters() {
        // "\u{91d1}\u{5e01}" is the Chinese for "coins": long values of multi-byte text are kept whole.
        let value = "\u{91d1}\u{5e01}🍓? $1 '".repeat(10_000);
        let expanded = expand_sql("SELECT ?", &[SqlValue::Text(value)]).unwrap();
        assert_eq!(expanded.matches("\u{91d1}\u{5e01}🍓").count(), 10_000);
        assert_eq!(expanded.matches("''").count(), 10_000);
        let values: Vec<_> = (0..200).map(SqlValue::Integer).collect();
        let sql = format!("SELECT {}", vec!["?"; values.len()].join(", "));
        let expected = format!(
            "SELECT {}",
            (0..200)
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        assert_eq!(expand_sql(&sql, &values).unwrap(), expected);
    }
}
