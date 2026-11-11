//! Fixed rewriting of MySQL text into SQLite (docs/architecture.md, "Data").
//!
//! Business SQL is written once as MySQL text and runs unchanged on MySQL; on SQLite it first goes through the
//! 8 rules here:
//!
//! 1. Drop the locking read `FOR UPDATE`: an SQLite write transaction takes the write lock at its start
//!    (`BEGIN IMMEDIATE`), so every read in the transaction is under the lock.
//! 2. Drop `LOCK IN SHARE MODE` and `FOR SHARE`, for the same reason.
//! 3. `GREATEST(` becomes `max(`: SQLite's multi-argument `max` is a scalar function and likewise returns NULL
//!    when an argument is NULL.
//! 4. `FORCE INDEX (name)` becomes `INDEXED BY table__name` (SQLite index names are always "table__original name"), so
//!    both sides use the same narrow index (if it were only dropped, SQLite would pick
//!    another index and build a temporary B-tree to sort); `FORCE INDEX (PRIMARY)` and `IGNORE INDEX (…)` are dropped
//!    and left to the planner.
//! 5. `STRAIGHT_JOIN`: dropped right after `SELECT`; between tables it becomes `CROSS JOIN`, which also fixes the
//!    join order in SQLite.
//! 6. `CAST(… AS SIGNED)` becomes `AS INTEGER`.
//! 7. `ON DUPLICATE KEY UPDATE` becomes `ON CONFLICT DO UPDATE SET`, with `VALUES(col)` inside it becoming
//!    `excluded.col`. Without a conflict target SQLite takes this branch on any unique constraint conflict, the
//!    same as MySQL. The affected row count differs: MySQL reports 2 when it changes an existing row to new values,
//!    SQLite reports 1; no current caller looks at this number (checked one by one).
//! 8. `IF(` becomes `iif(`, with the same meaning for the three arguments.
//!
//! Every other MySQL-specific construct is an error, never a guess: the call site has to give the SQLite text with
//! [`Query::sqlite`](super::Query::sqlite). Two constructs that SQLite can run but with different results are also
//! stopped: backslash escapes in strings (SQLite does not recognize them), and a later assignment in `SET` reading a
//! column changed by an earlier one (a MySQL single-table UPDATE uses the new values in order, SQLite always uses
//! the old values).
use std::{borrow::Cow, fmt};

/// A MySQL construct the rewriter does not recognize.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported {
    construct: String,
    sql: String,
}

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const SHOWN: usize = 240;
        let sql: String = self.sql.chars().take(SHOWN).collect();
        let more = if self.sql.chars().count() > SHOWN {
            "…"
        } else {
            ""
        };
        write!(
            f,
            "SQLite does not support this MySQL construct ({}); give the SQLite text with .sqlite(…) at the call site: {sql}{more}",
            self.construct
        )
    }
}

impl std::error::Error for Unsupported {}

impl From<Unsupported> for sqlx::Error {
    fn from(error: Unsupported) -> Self {
        sqlx::Error::Configuration(Box::new(error))
    }
}

/// Rewrite one MySQL statement into something SQLite can run; borrowed unchanged when nothing needs changing.
pub fn sqlite(sql: &str) -> Result<Cow<'_, str>, Unsupported> {
    let tokens = lex(sql).map_err(|construct| Unsupported {
        construct: construct.into(),
        sql: sql.to_owned(),
    })?;
    let mut rewrite = Rewrite::new(&tokens);
    rewrite.run().map_err(|construct| Unsupported {
        construct,
        sql: sql.to_owned(),
    })?;
    Ok(rewrite.output(sql))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// Whitespace and comments.
    Space,
    Word,
    /// An identifier in backticks.
    Quoted,
    Str,
    Number,
    Param,
    Punct,
}

#[derive(Clone, Copy, Debug)]
struct Token<'a> {
    kind: Kind,
    text: &'a str,
}

fn lex(sql: &str) -> Result<Vec<Token<'_>>, &'static str> {
    let bytes = sql.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < sql.len() {
        let rest = &sql[i..];
        let c = rest.chars().next().unwrap_or_default();
        let (kind, len) = if c.is_whitespace() {
            (
                Kind::Space,
                rest.find(|c: char| !c.is_whitespace())
                    .unwrap_or(rest.len()),
            )
        } else if rest.starts_with("/*!") {
            return Err("MySQL executable comment /*! */");
        } else if rest.starts_with("/*") {
            (
                Kind::Space,
                rest.find("*/").ok_or("unterminated comment")? + 2,
            )
        } else if rest.starts_with("--") && rest[2..].chars().next().is_none_or(char::is_whitespace)
        {
            (Kind::Space, rest.find('\n').unwrap_or(rest.len()))
        } else if c == '#' {
            return Err("# comment");
        } else if c == '\'' {
            let mut end = 1;
            loop {
                match bytes.get(i + end) {
                    None => return Err("unterminated string"),
                    Some(b'\\') => return Err("backslash escape in a string"),
                    Some(b'\'') if bytes.get(i + end + 1) == Some(&b'\'') => end += 2,
                    Some(b'\'') => break,
                    Some(_) => end += 1,
                }
            }
            (Kind::Str, end + 1)
        } else if c == '"' {
            return Err("double-quoted string");
        } else if c == '`' {
            let mut end = 1;
            loop {
                match bytes.get(i + end) {
                    None => return Err("unterminated backquoted identifier"),
                    Some(b'`') if bytes.get(i + end + 1) == Some(&b'`') => end += 2,
                    Some(b'`') => break,
                    Some(_) => end += 1,
                }
            }
            (Kind::Quoted, end + 1)
        } else if c == '?' {
            (Kind::Param, 1)
        } else if c == '@' {
            return Err("@ variable");
        } else if c.is_ascii_digit() {
            let len = if rest.starts_with("0x") || rest.starts_with("0X") {
                2 + rest[2..]
                    .find(|c: char| !c.is_ascii_hexdigit())
                    .unwrap_or(rest.len() - 2)
            } else {
                rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '.'))
                    .unwrap_or(rest.len())
            };
            (Kind::Number, len)
        } else if c.is_alphanumeric() || c == '_' || c == '$' {
            (
                Kind::Word,
                rest.find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
                    .unwrap_or(rest.len()),
            )
        } else {
            for (op, why) in [
                ("<=>", "<=> comparison"),
                ("->>", "->> JSON extraction"),
                ("->", "-> JSON extraction"),
                ("||", "|| operator"),
                ("&&", "&& operator"),
                (":=", ":= assignment"),
            ] {
                if rest.starts_with(op) {
                    return Err(why);
                }
            }
            let two = ["<=", ">=", "<>", "!=", "<<", ">>"]
                .into_iter()
                .find(|op| rest.starts_with(op));
            match (two, c) {
                (Some(op), _) => (Kind::Punct, op.len()),
                // MySQL's `/` yields a decimal; SQLite truncates when dividing two integers.
                (None, '/') => return Err("/ division"),
                (None, '!') => return Err("! negation"),
                (None, '^') => return Err("^ exclusive or"),
                (None, ';') => return Err("multiple statements"),
                (None, _) => (Kind::Punct, c.len_utf8()),
            }
        };
        tokens.push(Token {
            kind,
            text: &sql[i..i + len],
        });
        i += len;
    }
    Ok(tokens)
}

/// MySQL constructs that are errors when met (compared in upper case). `LEFT`, `RIGHT` and the like are only
/// functions when directly followed by a parenthesis; see [`FUNCTIONS`].
const REJECTED: &[&str] = &[
    "ANY_VALUE",
    "BENCHMARK",
    "BINARY",
    "CHARACTER",
    "CHARSET",
    "COLLATE",
    "CONCAT",
    "CONCAT_WS",
    "CONVERT",
    "CURDATE",
    "CURTIME",
    "DATEDIFF",
    "DATE_ADD",
    "DATE_FORMAT",
    "DATE_SUB",
    "DECIMAL",
    "DELAYED",
    "DIV",
    "DUAL",
    "DUPLICATE",
    "FIND_IN_SET",
    "FOUND_ROWS",
    "FROM_UNIXTIME",
    "GROUP_CONCAT",
    "HIGH_PRIORITY",
    "INET_ATON",
    "INET_NTOA",
    "INFORMATION_SCHEMA",
    "INTERVAL",
    "ISNULL",
    "LAST_INSERT_ID",
    "LEAST",
    "LOCATE",
    "LOW_PRIORITY",
    "LPAD",
    "MD5",
    "NOW",
    "NOWAIT",
    "PARTITION",
    "PROCESSLIST",
    "RAND",
    "REGEXP",
    "REPLACE",
    "RLIKE",
    "ROLLUP",
    "ROW_COUNT",
    "RPAD",
    "SHA1",
    "SHA2",
    "SLEEP",
    "SQL_BIG_RESULT",
    "SQL_BUFFER_RESULT",
    "SQL_CACHE",
    "SQL_CALC_FOUND_ROWS",
    "SQL_NO_CACHE",
    "SQL_SMALL_RESULT",
    "STR_TO_DATE",
    "SUBSTRING_INDEX",
    "SYSDATE",
    "TIMESTAMPADD",
    "TIMESTAMPDIFF",
    "TRUNCATE",
    "UNHEX",
    "UNIX_TIMESTAMP",
    "UNSIGNED",
    "UTC_DATE",
    "UTC_TIMESTAMP",
    "UUID",
    "XOR",
];

/// Functions that are errors when directly followed by a parenthesis: both sides have them but with different
/// meanings (`LENGTH` counts bytes in MySQL and characters in SQLite), or SQLite lacks them.
const FUNCTIONS: &[&str] = &[
    "CHAR",
    "CHAR_LENGTH",
    "CHARACTER_LENGTH",
    "ELT",
    "FIELD",
    "FORMAT",
    "HEX",
    "LEFT",
    "LENGTH",
    "MID",
    "MOD",
    "POSITION",
    "REPEAT",
    "RIGHT",
    "SPACE",
];

struct Rewrite<'t, 'a> {
    tokens: &'t [Token<'a>],
    /// Index in `tokens` of each non-whitespace token.
    sig: Vec<usize>,
    /// Parenthesis depth of each non-whitespace token (the `(` itself counts as the outer level).
    depth: Vec<usize>,
    /// By `tokens` index: `Some(text)` replaces it with this text, `Some("")` deletes it.
    edits: Vec<Option<Cow<'static, str>>>,
}

impl<'t, 'a> Rewrite<'t, 'a> {
    fn new(tokens: &'t [Token<'a>]) -> Self {
        let sig: Vec<usize> = (0..tokens.len())
            .filter(|&i| tokens[i].kind != Kind::Space)
            .collect();
        let mut depth = Vec::with_capacity(sig.len());
        let mut level = 0usize;
        for &i in &sig {
            match tokens[i].text {
                ")" => {
                    level = level.saturating_sub(1);
                    depth.push(level);
                }
                "(" => {
                    depth.push(level);
                    level += 1;
                }
                _ => depth.push(level),
            }
        }
        Self {
            tokens,
            sig,
            depth,
            edits: vec![None; tokens.len()],
        }
    }

    fn token(&self, p: usize) -> Option<Token<'a>> {
        self.sig.get(p).map(|&i| self.tokens[i])
    }

    fn word(&self, p: usize) -> Option<String> {
        self.token(p)
            .filter(|token| token.kind == Kind::Word)
            .map(|token| token.text.to_ascii_uppercase())
    }

    fn is(&self, p: usize, text: &str) -> bool {
        self.token(p).is_some_and(|token| {
            if token.kind == Kind::Word {
                token.text.eq_ignore_ascii_case(text)
            } else {
                token.kind == Kind::Punct && token.text == text
            }
        })
    }

    /// Position of the `)` matching the `(` at `p`.
    fn closing(&self, p: usize) -> Result<usize, String> {
        let level = self.depth[p];
        (p + 1..self.sig.len())
            .find(|&q| self.is(q, ")") && self.depth[q] == level)
            .ok_or_else(|| "unbalanced parentheses".to_owned())
    }

    fn replace(&mut self, p: usize, text: &'static str) {
        self.edits[self.sig[p]] = Some(Cow::Borrowed(text));
    }

    /// Delete the tokens `from..=to` together with the whitespace right before them.
    fn remove(&mut self, from: usize, to: usize) {
        let start = self.sig[from];
        if start > 0 && self.tokens[start - 1].kind == Kind::Space {
            self.edits[start - 1] = Some(Cow::Borrowed(""));
        }
        for i in start..=self.sig[to] {
            self.edits[i] = Some(Cow::Borrowed(""));
        }
    }

    fn run(&mut self) -> Result<(), String> {
        let first = self.word(0).unwrap_or_default();
        if !matches!(
            first.as_str(),
            "SELECT" | "INSERT" | "UPDATE" | "DELETE" | "WITH"
        ) {
            let shown = self.token(0).map_or("", |token| token.text);
            return Err(format!("statement starting with {shown}"));
        }
        let mut upsert = None;
        // INSERT … SELECT … ON CONFLICT: SQLite requires the SELECT to have a WHERE, otherwise it cannot tell
        // whether ON belongs to a join or to the upsert.
        let mut insert_select = false;
        let mut insert_where = false;
        let mut p = 0;
        while p < self.sig.len() {
            let token = self.token(p).expect("in range");
            let next_is_paren = self.is(p + 1, "(");
            if token.kind != Kind::Word {
                // `INSERT INTO t () VALUES ()` is MySQL syntax (SQLite writes DEFAULT VALUES); a zero-argument
                // function `f()` is let through as usual.
                if token.kind == Kind::Punct
                    && token.text == "("
                    && self.is(p + 1, ")")
                    && (p < 2 || self.is(p - 2, "INTO") || self.word(p - 1).is_none())
                {
                    return Err("empty parentheses ()".into());
                }
                p += 1;
                continue;
            }
            let word = token.text.to_ascii_uppercase();
            if first == "INSERT" && self.depth[p] == 0 {
                match word.as_str() {
                    "SELECT" => insert_select = true,
                    "WHERE" if insert_select => insert_where = true,
                    _ => (),
                }
            }
            if word.starts_with("JSON_") {
                return Err(format!("function {word}"));
            }
            if REJECTED.contains(&word.as_str()) {
                return Err(word);
            }
            if next_is_paren && FUNCTIONS.contains(&word.as_str()) {
                return Err(format!("function {word}"));
            }
            match word.as_str() {
                "FOR" => {
                    let what = self.word(p + 1).unwrap_or_default();
                    if what != "UPDATE" && what != "SHARE" {
                        return Err(format!("FOR {what}"));
                    }
                    if let Some(after) = self.word(p + 2)
                        && matches!(after.as_str(), "NOWAIT" | "SKIP" | "OF")
                    {
                        return Err(format!("FOR {what} {after}"));
                    }
                    self.remove(p, p + 1);
                    p += 2;
                }
                "LOCK" => {
                    if !(self.is(p + 1, "IN") && self.is(p + 2, "SHARE") && self.is(p + 3, "MODE"))
                    {
                        return Err("LOCK".into());
                    }
                    self.remove(p, p + 3);
                    p += 4;
                }
                "GREATEST" if next_is_paren => {
                    self.replace(p, "max");
                    p += 1;
                }
                "FORCE" | "IGNORE" | "USE" => {
                    let index = self.is(p + 1, "INDEX") || self.is(p + 1, "KEY");
                    if word == "USE" || !index || !self.is(p + 2, "(") {
                        let after = self.word(p + 1).unwrap_or_default();
                        return Err(format!("{word} {after}"));
                    }
                    let end = self.closing(p + 2)?;
                    if (p + 3..end).any(|q| {
                        !matches!(
                            self.token(q).map(|t| t.kind),
                            Some(Kind::Word | Kind::Quoted)
                        ) && !self.is(q, ",")
                    }) {
                        return Err(format!("{word} INDEX without an index name"));
                    }
                    let names: Vec<&str> = (p + 3..end)
                        .filter(|&q| !self.is(q, ","))
                        .filter_map(|q| self.token(q).map(|t| unquote(t.text)))
                        .collect();
                    match names.as_slice() {
                        // The primary key and IGNORE INDEX are dropped and left to the planner.
                        [name] if word == "FORCE" && !name.eq_ignore_ascii_case("PRIMARY") => {
                            let table = self.indexed_table(p)?;
                            self.edits[self.sig[p]] =
                                Some(Cow::Owned(format!("INDEXED BY {table}__{name}")));
                            for i in self.sig[p] + 1..=self.sig[end] {
                                self.edits[i] = Some(Cow::Borrowed(""));
                            }
                        }
                        [_, _, ..] if word == "FORCE" => {
                            return Err("FORCE INDEX with several indexes".into());
                        }
                        _ => self.remove(p, end),
                    }
                    p = end + 1;
                }
                "STRAIGHT_JOIN" => {
                    if p > 0 && self.is(p - 1, "SELECT") {
                        self.remove(p, p);
                    } else {
                        self.replace(p, "CROSS JOIN");
                    }
                    p += 1;
                }
                "SIGNED" => {
                    if !(p > 0 && self.is(p - 1, "AS")) {
                        return Err("SIGNED".into());
                    }
                    self.replace(p, "INTEGER");
                    p += 1;
                }
                "ON" if self.is(p + 1, "DUPLICATE") => {
                    if !(self.is(p + 2, "KEY") && self.is(p + 3, "UPDATE")) {
                        return Err("ON DUPLICATE".into());
                    }
                    if first != "INSERT" || self.depth[p] != 0 {
                        return Err("ON DUPLICATE KEY UPDATE not at the end of an INSERT".into());
                    }
                    if insert_select && !insert_where {
                        return Err(
                            "ON DUPLICATE KEY UPDATE on INSERT … SELECT without WHERE".into()
                        );
                    }
                    self.remove(p + 1, p + 3);
                    self.edits[self.sig[p]] = Some(Cow::Borrowed("ON CONFLICT DO UPDATE SET"));
                    upsert = Some(p + 4);
                    p += 4;
                }
                "VALUES" if upsert.is_some() => {
                    // excluded.col: only `VALUES(col)` is recognized.
                    let column = self.token(p + 2);
                    if !(next_is_paren
                        && column.is_some_and(|t| matches!(t.kind, Kind::Word | Kind::Quoted))
                        && self.is(p + 3, ")"))
                    {
                        return Err("VALUES inside ON DUPLICATE KEY UPDATE".into());
                    }
                    let column = column.expect("checked above").text;
                    self.edits[self.sig[p]] = Some(Cow::Owned(format!("excluded.{column}")));
                    for q in p + 1..=p + 3 {
                        self.edits[self.sig[q]] = Some(Cow::Borrowed(""));
                    }
                    p += 4;
                }
                "VALUES" if next_is_paren && self.is(p + 2, ")") => {
                    return Err("empty parentheses ()".into());
                }
                "IF" => {
                    if !next_is_paren {
                        return Err("IF".into());
                    }
                    self.replace(p, "iif");
                    p += 1;
                }
                "UPDATE" if p == 0 => {
                    self.check_update()?;
                    p += 1;
                }
                "DELETE" if p == 0 => {
                    if !self.is(1, "FROM") {
                        return Err("multi-table DELETE".into());
                    }
                    self.check_no_order_or_limit("DELETE")?;
                    p += 1;
                }
                _ => p += 1,
            }
        }
        if let Some(start) = upsert {
            self.check_assignments(start, self.sig.len(), true)?;
        }
        Ok(())
    }

    /// The table name before `FORCE INDEX`: `table FORCE`, `table alias FORCE`, `table AS alias FORCE`, with FROM,
    /// JOIN or a comma before the table name.
    fn indexed_table(&self, p: usize) -> Result<&'a str, String> {
        let starts = |q: usize| {
            self.is(q, "FROM")
                || self.is(q, "JOIN")
                || self.is(q, "STRAIGHT_JOIN")
                || self.is(q, ",")
        };
        let table = if p >= 2 && starts(p - 2) {
            p - 1
        } else if p >= 4 && self.is(p - 2, "AS") && starts(p - 4) {
            p - 3
        } else if p >= 3 && starts(p - 3) {
            p - 2
        } else {
            return Err("cannot tell which table FORCE INDEX belongs to".into());
        };
        self.token(table)
            .filter(|t| matches!(t.kind, Kind::Word | Kind::Quoted))
            .map(|t| unquote(t.text))
            .ok_or_else(|| "cannot tell which table FORCE INDEX belongs to".to_owned())
    }

    /// Single-table UPDATE: no joins, ORDER BY or LIMIT; a later assignment in SET must not read a column changed
    /// by an earlier one.
    fn check_update(&self) -> Result<(), String> {
        let set = (1..self.sig.len())
            .find(|&q| self.depth[q] == 0 && self.is(q, "SET"))
            .ok_or("UPDATE without SET")?;
        if (1..set).any(|q| self.depth[q] == 0 && (self.is(q, "JOIN") || self.is(q, ","))) {
            return Err("multi-table UPDATE".into());
        }
        // An SQLite UPDATE alias must be written with AS: only `UPDATE table` and `UPDATE table AS alias` are
        // recognized.
        if !(set == 2 || (set == 4 && self.is(2, "AS"))) {
            return Err("an UPDATE table alias needs AS".into());
        }
        self.check_no_order_or_limit("UPDATE")?;
        let end = (set + 1..self.sig.len())
            .find(|&q| self.depth[q] == 0 && self.is(q, "WHERE"))
            .unwrap_or(self.sig.len());
        self.check_assignments(set + 1, end, false)
    }

    fn check_no_order_or_limit(&self, statement: &str) -> Result<(), String> {
        if (1..self.sig.len())
            .any(|q| self.depth[q] == 0 && (self.is(q, "ORDER") || self.is(q, "LIMIT")))
        {
            return Err(format!("{statement} with ORDER BY or LIMIT"));
        }
        Ok(())
    }

    /// `a = …, b = …`: an error when a later expression reads a column assigned earlier. Columns inside `VALUES(col)`
    /// and subqueries do not count.
    fn check_assignments(&self, start: usize, end: usize, upsert: bool) -> Result<(), String> {
        let level = self.depth.get(start).copied().unwrap_or(0);
        let mut assigned: Vec<String> = Vec::new();
        let mut q = start;
        while q < end {
            // Assignment target: `col` or `alias.col`, followed by `=`.
            let mut target = q;
            while self.is(target + 1, ".") {
                target += 2;
            }
            if target != q {
                return Err("a SET column qualified with a table name".into());
            }
            if !self.is(target + 1, "=") {
                return Err("unrecognized SET assignment".into());
            }
            let column = self
                .token(target)
                .map(|t| unquote(t.text).to_ascii_lowercase())
                .unwrap_or_default();
            let mut r = target + 2;
            while r < end && !(self.depth[r] == level && self.is(r, ",")) {
                let token = self.token(r).expect("in range");
                if self.is(r, "(") && self.is(r + 1, "SELECT") {
                    r = self.closing(r)? + 1;
                    continue;
                }
                if upsert && self.is(r, "VALUES") && self.is(r + 1, "(") {
                    r = self.closing(r + 1)? + 1;
                    continue;
                }
                if matches!(token.kind, Kind::Word | Kind::Quoted)
                    && !self.is(r + 1, "(")
                    && assigned.contains(&unquote(token.text).to_ascii_lowercase())
                {
                    return Err(format!(
                        "the SET expression of {column} reads column {} changed earlier (MySQL sees the new value, SQLite the old one)",
                        token.text
                    ));
                }
                r += 1;
            }
            assigned.push(column);
            q = r + 1;
        }
        Ok(())
    }

    fn output(&self, sql: &'a str) -> Cow<'a, str> {
        if self.edits.iter().all(Option::is_none) {
            return Cow::Borrowed(sql);
        }
        let mut out = String::with_capacity(sql.len());
        for (token, edit) in self.tokens.iter().zip(&self.edits) {
            out.push_str(edit.as_deref().unwrap_or(token.text));
        }
        Cow::Owned(out)
    }
}

fn unquote(text: &str) -> &str {
    text.strip_prefix('`')
        .and_then(|text| text.strip_suffix('`'))
        .unwrap_or(text)
}

#[cfg(test)]
mod tests {
    use super::sqlite;

    fn ok(sql: &str) -> String {
        sqlite(sql)
            .unwrap_or_else(|error| panic!("{error}"))
            .into_owned()
    }

    fn rejected(sql: &str) -> String {
        sqlite(sql).expect_err(sql).construct
    }

    #[test]
    fn leaves_portable_sql_untouched_and_borrowed() {
        let sql = "SELECT t.tid, `key` FROM gonof_forum_thread t LEFT JOIN gonof_forum_post p ON p.tid = t.tid \
                   WHERE t.subject LIKE ? ESCAPE '!' AND t.note = 'it''s FOR UPDATE' ORDER BY t.tid DESC LIMIT ?, ?";
        assert!(matches!(sqlite(sql), Ok(std::borrow::Cow::Borrowed(_))));
    }

    #[test]
    fn strips_locking_reads_and_index_hints() {
        assert_eq!(
            ok("SELECT uid FROM gonof_common_member WHERE uid=? FOR UPDATE"),
            "SELECT uid FROM gonof_common_member WHERE uid=?"
        );
        assert_eq!(
            ok(
                "SELECT 1 FROM t WHERE (SELECT 1 FROM u WHERE id=? LOCK IN SHARE MODE) LOCK IN SHARE MODE"
            ),
            "SELECT 1 FROM t WHERE (SELECT 1 FROM u WHERE id=?)"
        );
        assert_eq!(ok("SELECT a FROM t FOR SHARE"), "SELECT a FROM t");
        assert_eq!(
            ok("SELECT skey FROM gonof_common_settings FORCE INDEX (PRIMARY) WHERE skey IN (?,?)"),
            "SELECT skey FROM gonof_common_settings WHERE skey IN (?,?)"
        );
        assert_eq!(
            ok("SELECT a FROM t IGNORE INDEX (`i1`, i2) JOIN u FORCE KEY(i3) ON u.a=t.a"),
            "SELECT a FROM t JOIN u INDEXED BY u__i3 ON u.a=t.a"
        );
        assert_eq!(
            ok(
                "SELECT t.tid FROM gonof_forum_thread t FORCE INDEX (gonof_thread_latest) WHERE t.fid=? ORDER BY t.lastpost DESC"
            ),
            "SELECT t.tid FROM gonof_forum_thread t INDEXED BY gonof_forum_thread__gonof_thread_latest WHERE t.fid=? ORDER BY t.lastpost DESC"
        );
        assert_eq!(
            ok("SELECT 1 FROM x AS y FORCE INDEX (`i`) WHERE y.a=1"),
            "SELECT 1 FROM x AS y INDEXED BY x__i WHERE y.a=1"
        );
        assert_eq!(
            ok("SELECT 1 FROM x FORCE INDEX (i) JOIN z ON z.a=x.a"),
            "SELECT 1 FROM x INDEXED BY x__i JOIN z ON z.a=x.a"
        );
        assert_eq!(
            rejected("SELECT a FROM t FORCE INDEX (i1, i2)"),
            "FORCE INDEX with several indexes"
        );
        assert_eq!(
            rejected("SELECT a FROM t FOR UPDATE SKIP LOCKED"),
            "FOR UPDATE SKIP"
        );
        assert_eq!(
            rejected("SELECT a FROM t FORCE INDEX FOR ORDER BY (i)"),
            "FORCE INDEX"
        );
        assert_eq!(rejected("SELECT a FROM t USE INDEX (i)"), "USE INDEX");
    }

    #[test]
    fn renames_functions_and_casts() {
        assert_eq!(
            ok("UPDATE t SET a=GREATEST(dateline, ?), b=IF(lastpost<=?, ?, b) WHERE id=?"),
            "UPDATE t SET a=max(dateline, ?), b=iif(lastpost<=?, ?, b) WHERE id=?"
        );
        assert_eq!(
            ok("SELECT CAST(SUM(l.delta) AS SIGNED) FROM l"),
            "SELECT CAST(SUM(l.delta) AS INTEGER) FROM l"
        );
        assert_eq!(
            ok("SELECT STRAIGHT_JOIN p.uid FROM p STRAIGHT_JOIN m ON m.uid=p.uid"),
            "SELECT p.uid FROM p CROSS JOIN m ON m.uid=p.uid"
        );
    }

    #[test]
    fn turns_on_duplicate_key_into_upserts() {
        assert_eq!(
            ok(
                "INSERT INTO s(skey,svalue) VALUES(?,?),(?,?) ON DUPLICATE KEY UPDATE svalue=VALUES(svalue)"
            ),
            "INSERT INTO s(skey,svalue) VALUES(?,?),(?,?) ON CONFLICT DO UPDATE SET svalue=excluded.svalue"
        );
        assert_eq!(
            ok(
                "INSERT INTO h(username,expires_at) VALUES(?,?) ON DUPLICATE KEY UPDATE expires_at=GREATEST(expires_at,VALUES(expires_at))"
            ),
            "INSERT INTO h(username,expires_at) VALUES(?,?) ON CONFLICT DO UPDATE SET expires_at=max(expires_at,excluded.expires_at)"
        );
        assert_eq!(
            ok("INSERT INTO c(uid) VALUES(?) ON DUPLICATE KEY UPDATE uid=uid"),
            "INSERT INTO c(uid) VALUES(?) ON CONFLICT DO UPDATE SET uid=uid"
        );
        assert_eq!(
            ok(
                "INSERT INTO p(uid,last_seen) SELECT uid,MAX(t) FROM s WHERE uid=? GROUP BY uid ON DUPLICATE KEY UPDATE last_seen = GREATEST(last_seen, VALUES(last_seen))"
            ),
            "INSERT INTO p(uid,last_seen) SELECT uid,MAX(t) FROM s WHERE uid=? GROUP BY uid ON CONFLICT DO UPDATE SET last_seen = max(last_seen, excluded.last_seen)"
        );
        assert_eq!(
            rejected("INSERT INTO p(uid) SELECT uid FROM s ON DUPLICATE KEY UPDATE uid=uid"),
            "ON DUPLICATE KEY UPDATE on INSERT … SELECT without WHERE"
        );
        assert_eq!(
            rejected("INSERT INTO c(a,b) VALUES(?,?) ON DUPLICATE KEY UPDATE a=VALUES(a)+1, b=a"),
            "the SET expression of b reads column a changed earlier (MySQL sees the new value, SQLite the old one)"
        );
    }

    #[test]
    fn rejects_constructs_that_need_a_hand_written_version() {
        for (sql, construct) in [
            (
                "SELECT JSON_UNQUOTE(JSON_EXTRACT(data,'$.a')) FROM l",
                "function JSON_UNQUOTE",
            ),
            ("SELECT CAST(UNIX_TIMESTAMP() AS SIGNED)", "UNIX_TIMESTAMP"),
            ("SELECT LEFT(message,255) FROM p", "function LEFT"),
            ("UPDATE o SET label=CONCAT('a',n)", "CONCAT"),
            ("SELECT a COLLATE utf8mb4_bin FROM t", "COLLATE"),
            ("INSERT IGNORE INTO t(a) VALUES(?)", "IGNORE INTO"),
            ("INSERT INTO t () VALUES ()", "empty parentheses ()"),
            ("INSERT INTO t(a) VALUES ()", "empty parentheses ()"),
            (
                "REPLACE INTO t(a) VALUES(?)",
                "statement starting with REPLACE",
            ),
            ("SET FOREIGN_KEY_CHECKS = 0", "statement starting with SET"),
            (
                "UPDATE t a JOIN u b ON b.id=a.id SET a.x=b.x",
                "multi-table UPDATE",
            ),
            (
                "DELETE FROM s WHERE t < ? ORDER BY t LIMIT 1000",
                "DELETE with ORDER BY or LIMIT",
            ),
            ("DELETE a FROM a JOIN b ON b.id=a.id", "multi-table DELETE"),
            (
                "UPDATE t SET a=?, b=a+1 WHERE id=?",
                "the SET expression of b reads column a changed earlier (MySQL sees the new value, SQLite the old one)",
            ),
            (
                "UPDATE t n SET b=1 WHERE n.a=?",
                "an UPDATE table alias needs AS",
            ),
            (
                "UPDATE t AS n SET n.b=1 WHERE n.a=?",
                "a SET column qualified with a table name",
            ),
            ("SELECT 'a\\'b'", "backslash escape in a string"),
            ("SELECT \"a\"", "double-quoted string"),
            ("SELECT a/2 FROM t", "/ division"),
            ("SELECT a FROM t WHERE b <=> ?", "<=> comparison"),
            ("SELECT 1; SELECT 2", "multiple statements"),
            ("SELECT LENGTH(a) FROM t", "function LENGTH"),
        ] {
            assert_eq!(rejected(sql), construct, "{sql}");
        }
        // Columns and keywords with the same name are let through as usual.
        ok("SELECT p.first, `left`, t.char_count FROM p LEFT JOIN t ON t.id = p.id");
        ok("SELECT sqlite_version()");
        ok("UPDATE t SET a=a+1, b=(SELECT COUNT(*) FROM u WHERE u.a=t.id) WHERE id=?");
        ok("UPDATE t AS n SET b=n.b+1 WHERE n.a=?");
    }
}
