//! Prepares every static SQL statement on SQLite.
//!
//! Collects the SQL literal of every whole statement in production code (starting with SELECT, INSERT, UPDATE,
//! DELETE or WITH, and without `format!` braces): when it is directly followed by `.sqlite(…)`, the hand-written
//! SQLite text is taken, otherwise it goes through the rewriter. Statements the rewriter rejects must carry a
//! hand-written version. The resulting text is `prepare`d on a migrated SQLite database, so syntax errors and
//! wrong table or column names fail here. Dynamic SQL built with `format!` is covered by the full test suite.
use super::{Database, dialect, source_guards::production_sources};
use crate::config::DatabaseConfig;

/// A Rust literal: its start and end bytes and its decoded content.
struct Literal {
    start: usize,
    end: usize,
    text: String,
}

/// Extracts the string literals from source code: ordinary strings (with escapes and `\` line continuations) and
/// raw strings. Skips comments and char literals.
fn literals(source: &str) -> Vec<Literal> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let rest = &source[i..];
        if rest.starts_with("//") {
            i += rest.find('\n').unwrap_or(rest.len());
            continue;
        }
        if rest.starts_with("/*") {
            i += rest.find("*/").map_or(rest.len(), |end| end + 2);
            continue;
        }
        let ident_before = i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
        let raw = rest
            .strip_prefix("br")
            .or_else(|| rest.strip_prefix('r'))
            .filter(|_| !ident_before);
        if let Some(after) = raw {
            let hashes = after.len() - after.trim_start_matches('#').len();
            if after[hashes..].starts_with('"') {
                let open = rest.len() - after.len() + hashes + 1;
                let close = format!("\"{}", "#".repeat(hashes));
                let body = &rest[open..];
                let length = body.find(&close).expect("unterminated raw string");
                out.push(Literal {
                    start: i,
                    end: i + open + length + close.len(),
                    text: body[..length].to_owned(),
                });
                i += open + length + close.len();
                continue;
            }
        }
        if bytes[i] == b'\'' {
            // Char literals 'x', '\n', '\u{..}'; a lifetime 'a has no closing quote and is skipped as one character.
            let close = rest[1..]
                .char_indices()
                .take(12)
                .find(|&(_, c)| c == '\'')
                .map(|(offset, _)| offset + 2);
            let is_char = close.is_some_and(|close| {
                let inner = &rest[1..close - 1];
                inner.chars().count() == 1 || inner.starts_with('\\')
            });
            i += if is_char { close.unwrap() } else { 1 };
            continue;
        }
        if bytes[i] == b'"' {
            let mut text = String::new();
            let mut j = i + 1;
            loop {
                let c = source[j..].chars().next().expect("unterminated string");
                match c {
                    '"' => break,
                    '\\' => {
                        let next = source[j + 1..].chars().next().unwrap();
                        j += 1 + next.len_utf8();
                        match next {
                            '\n' => {
                                while source[j..].starts_with(char::is_whitespace) {
                                    j += source[j..].chars().next().unwrap().len_utf8();
                                }
                            }
                            'n' => text.push('\n'),
                            't' => text.push('\t'),
                            'r' => text.push('\r'),
                            '0' => text.push('\0'),
                            '\\' | '"' | '\'' => text.push(next),
                            'x' => {
                                let code = u8::from_str_radix(&source[j..j + 2], 16).unwrap();
                                text.push(char::from(code));
                                j += 2;
                            }
                            'u' => {
                                let close = source[j..].find('}').unwrap();
                                let code =
                                    u32::from_str_radix(&source[j + 1..j + close], 16).unwrap();
                                text.push(char::from_u32(code).unwrap());
                                j += close + 1;
                            }
                            other => panic!("unhandled escape \\{other}"),
                        }
                    }
                    _ => {
                        text.push(c);
                        j += c.len_utf8();
                    }
                }
            }
            out.push(Literal {
                start: i,
                end: j + 1,
                text,
            });
            i = j + 1;
            continue;
        }
        i += rest.chars().next().map_or(1, char::len_utf8);
    }
    out
}

/// Files not scanned: the rewriter and this test's own samples, plus the debug panel that runs EXPLAIN only on
/// MySQL (its literals are keyword tables and hint texts).
const SKIPPED_FILES: &[&str] = &[
    "db/dialect.rs",
    "db/sqlite_statements.rs",
    "debug/explain.rs",
];

/// Statements executed only on the MySQL branch: the version query when connected to MySQL, and the database
/// name check of the profile import (the SQLite branch sends no SQL).
const MYSQL_ONLY: &[(&str, &str)] = &[
    ("db/mod.rs", "SELECT VERSION()"),
    ("content/profiles.rs", "SELECT DATABASE()"),
    (
        "db/mod.rs",
        "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = DATABASE()",
    ),
    (
        "db/mod.rs",
        "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = DATABASE() AND table_name = '_sqlx_migrations'",
    ),
];

fn is_statement(text: &str) -> bool {
    let trimmed = text.trim();
    let head: String = trimmed
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect::<String>()
        .to_ascii_uppercase();
    matches!(
        head.as_str(),
        "SELECT" | "INSERT" | "UPDATE" | "DELETE" | "WITH"
    ) && trimmed.contains(char::is_whitespace)
        && !text.contains(['{', '}'])
}

/// Fragments used for concatenation: starting with `String::from("…")` and followed by `push_str`, `"…".into()`
/// handed to the caller to continue building, or ending in `IN (` with the placeholders for this batch of keys
/// added by `content::copies::update_keys`.
fn is_fragment(source: &str, literal: &Literal) -> bool {
    source[..literal.start]
        .trim_end()
        .ends_with("String::from(")
        || source[literal.end..].trim_start().starts_with(".into()")
        || literal.text.trim_end().ends_with("IN (")
}

/// Skips whitespace and `//` comments.
fn skip_blank(mut text: &str) -> &str {
    loop {
        text = text.trim_start();
        match text.strip_prefix("//") {
            Some(comment) => text = comment.find('\n').map_or("", |end| &comment[end..]),
            None => return text,
        }
    }
}

/// The hand-written text inside the `).sqlite(` that directly follows a literal (not counted when `query(…)`
/// carries other arguments).
fn twin<'a>(source: &str, literal: &Literal, all: &'a [Literal]) -> Option<&'a Literal> {
    let after = skip_blank(&source[literal.end..]);
    let after = skip_blank(after.strip_prefix(',').unwrap_or(after));
    let after = skip_blank(after.strip_prefix(')')?);
    let after = skip_blank(after.strip_prefix(".sqlite(")?);
    let at = source.len() - after.len();
    all.iter().find(|candidate| candidate.start == at)
}

fn line_of(source: &str, offset: usize) -> usize {
    source[..offset].matches('\n').count() + 1
}

#[tokio::test]
async fn every_static_statement_prepares_on_sqlite() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "gonof_sqlite_statements_{}_{nanos}.db",
        std::process::id()
    ));
    struct Remove(std::path::PathBuf);
    impl Drop for Remove {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
            }
        }
    }
    let _remove = Remove(path.clone());
    let config = DatabaseConfig {
        url: format!("sqlite://{}", path.display()),
        max_connections: 2,
        acquire_timeout_seconds: 10,
    };
    let db = Database::connect_creating(&config).await.unwrap();
    db.migrate().await.unwrap();
    let mut connection = db.pool.as_sqlite().unwrap().acquire().await.unwrap();

    let mut checked = 0;
    let mut twins = 0;
    let mut failures = Vec::new();
    let mut mysql_only_seen = Vec::new();
    for (file, source) in production_sources() {
        if SKIPPED_FILES.contains(&file.as_str()) {
            continue;
        }
        let all = literals(&source);
        let twin_starts: Vec<usize> = all
            .iter()
            .filter_map(|literal| twin(&source, literal, &all).map(|twin| twin.start))
            .collect();
        for literal in &all {
            if !is_statement(&literal.text)
                || twin_starts.contains(&literal.start)
                || is_fragment(&source, literal)
            {
                continue;
            }
            if MYSQL_ONLY.contains(&(file.as_str(), literal.text.as_str())) {
                mysql_only_seen.push((file.clone(), literal.text.clone()));
                continue;
            }
            let at = format!("{file}:{}", line_of(&source, literal.start));
            let sql = match twin(&source, literal, &all) {
                Some(twin) => {
                    twins += 1;
                    twin.text.clone()
                }
                None => match dialect::sqlite(&literal.text) {
                    Ok(sql) => sql.into_owned(),
                    Err(error) => {
                        failures.push(format!("{at}: {error}"));
                        continue;
                    }
                },
            };
            checked += 1;
            if let Err(error) = sqlx::Executor::prepare(&mut *connection, sql.as_str()).await {
                failures.push(format!("{at}: {error}\n    {sql}"));
            }
        }
    }
    drop(connection);
    db.pool.close().await;
    for (file, sql) in MYSQL_ONLY {
        assert!(
            mysql_only_seen.iter().any(|(f, s)| f == file && s == sql),
            "{file}: {sql} in MYSQL_ONLY is no longer in the source; remove the entry"
        );
    }
    assert!(
        failures.is_empty(),
        "{} statements fail on SQLite:\n{}",
        failures.len(),
        failures.join("\n")
    );
    eprintln!("prepared {checked} statements on SQLite, {twins} of them hand-written");
    // The count only guards against a broken scanner (reading nothing at all would also "all pass"). No hand-written
    // version is left at present; the scanning of hand-written versions is pinned by the self-test below.
    assert!(checked > 900, "only {checked} statements were checked");
}

#[test]
fn literal_scanner_reads_escapes_raw_strings_and_twins() {
    let source = r####"
        // "not a literal"
        let c = '"'; let l: &'static str = "a\"b\
              c";
        query("SELECT 1 FROM t WHERE a = ?").sqlite("SELECT 2").bind(x);
        query(r#"UPDATE t SET a = 'x"y'"#,)
            // comment
            .sqlite(r"x");
        query_scalar("SELECT {x}");
    "####;
    let found: Vec<String> = literals(source).into_iter().map(|l| l.text).collect();
    assert_eq!(
        found,
        [
            "a\"bc",
            "SELECT 1 FROM t WHERE a = ?",
            "SELECT 2",
            "UPDATE t SET a = 'x\"y'",
            "x",
            "SELECT {x}"
        ]
    );
    let all = literals(source);
    assert_eq!(
        twin(source, &all[1], &all).map(|t| t.text.as_str()),
        Some("SELECT 2")
    );
    assert_eq!(
        twin(source, &all[3], &all).map(|t| t.text.as_str()),
        Some("x")
    );
    assert!(twin(source, &all[5], &all).is_none());
    assert!(
        is_statement(&all[1].text) && !is_statement(&all[5].text) && !is_statement(&all[0].text)
    );
}
