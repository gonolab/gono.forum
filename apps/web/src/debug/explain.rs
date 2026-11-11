//! Request-local, best-effort planning diagnostics. Original bound SQL is only ever prefixed
//! with EXPLAIN, never ANALYZE. The displayed expanded SQL is not an execution input.
//!
//! Uses MySQL FORMAT=TRADITIONAL on the existing pool, or SQLite EXPLAIN QUERY PLAN on the
//! read-only pool. Planning has a separate observation scope, a strict SELECT gate, and bounded
//! connection/query time.
use std::time::{Duration, Instant};

use anyhow::Context;
use sqlx::{Column, Row, mysql::MySqlRow, sqlite::SqliteRow};

use crate::db::{
    Backend, Database,
    sql_display::{SqlValue, expand_sql_for},
};

use super::{
    trace,
    trace::{QueryRecord, QuerySource},
};

const PER_QUERY_BUDGET: Duration = Duration::from_millis(150);
const TOTAL_BUDGET: Duration = Duration::from_millis(750);
const MAX_QUERIES: usize = 32;
const MAX_SQL_BYTES: usize = 65_536;
const MAX_PARAMETER_BYTES: usize = 65_536;

#[derive(Clone, Debug)]
pub struct IndexUsage {
    pub status: &'static str,
    pub summary: String,
}

impl IndexUsage {
    fn skipped(reason: &str) -> Self {
        Self {
            status: "skipped",
            summary: crate::i18n::current().debug_explain_skipped(reason),
        }
    }

    fn unavailable(reason: &str) -> Self {
        Self {
            status: "unavailable",
            summary: crate::i18n::current().debug_explain_unavailable(single_line(reason)),
        }
    }
}

/// Keeps its own observation boundary so connection initialization is excluded as well.
pub async fn collect(database: &Database, records: &[QueryRecord]) -> Vec<IndexUsage> {
    trace::without(collect_inner(database, records)).await
}

async fn collect_inner(database: &Database, records: &[QueryRecord]) -> Vec<IndexUsage> {
    let started = Instant::now();
    let mut cache: Vec<(&QueryRecord, IndexUsage)> = Vec::new();
    let mut results = Vec::with_capacity(records.len());
    for record in records {
        if let Some(reason) = skipped_reason(record) {
            results.push(IndexUsage::skipped(reason));
            continue;
        }
        if let Some((_, usage)) = cache.iter().find(|(previous, _)| {
            previous.sql == record.sql && previous.parameters == record.parameters
        }) {
            results.push(usage.clone());
            continue;
        }
        let remaining = TOTAL_BUDGET.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            results.push(IndexUsage::unavailable(
                crate::i18n::current().debug_explain_budget_spent(),
            ));
            continue;
        }
        if cache.len() >= MAX_QUERIES {
            results.push(IndexUsage::skipped(
                crate::i18n::current().debug_explain_query_limit(),
            ));
            continue;
        }
        let usage = explain_one(database, record, remaining.min(PER_QUERY_BUDGET)).await;
        cache.push((record, usage.clone()));
        results.push(usage);
    }
    results
}

fn skipped_reason(record: &QueryRecord) -> Option<&'static str> {
    if record.source == QuerySource::Driver {
        return Some(crate::i18n::current().debug_explain_driver());
    }
    if record.error.is_some() {
        return Some(crate::i18n::current().debug_explain_failed());
    }
    if record.cancelled {
        return Some(crate::i18n::current().debug_explain_cancelled());
    }
    if record.expansion_error.is_some() {
        return Some(crate::i18n::current().debug_explain_parameters());
    }
    if record.sql.len() > MAX_SQL_BYTES {
        return Some(crate::i18n::current().debug_explain_sql_too_long());
    }
    let parameter_size = record.parameters.iter().try_fold(0_usize, |total, value| {
        total.checked_add(match value {
            SqlValue::Text(value) => value.len(),
            SqlValue::Bytes(value) => value.len(),
            _ => 8,
        })
    });
    if parameter_size.is_none_or(|size| size > MAX_PARAMETER_BYTES) {
        return Some(crate::i18n::current().debug_explain_parameters_too_long());
    }
    if expand_sql_for(record.backend, &record.sql, &record.parameters).is_err() {
        return Some(crate::i18n::current().debug_explain_parameters());
    }
    safe_select(&record.sql).err()
}

/// This deliberately small lexer is a whitelist gate, not a SQL rewriter. It rejects
/// unfamiliar syntax rather than pretending to prove that an arbitrary query is read-only.
/// Unknown functions are excluded because optimizers can evaluate functions while planning.
fn safe_select(sql: &str) -> Result<(), &'static str> {
    #[derive(Clone)]
    enum Token {
        Word(String),
        Identifier,
        Open,
        Dot,
        Other,
    }
    let bytes = sql.as_bytes();
    let mut tokens = Vec::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if byte.is_ascii_whitespace() {
            cursor += 1;
            continue;
        }
        if byte == b';' {
            if !bytes[cursor + 1..].iter().all(u8::is_ascii_whitespace) {
                return Err(crate::i18n::current().debug_explain_multiple());
            }
            break;
        }
        if bytes[cursor..].starts_with(b"--") || bytes[cursor..].starts_with(b"/*") || byte == b'#'
        {
            return Err(crate::i18n::current().debug_explain_comment());
        }
        if matches!(byte, b'\'' | b'"' | b'`') {
            let quote = byte;
            cursor += 1;
            let mut closed = false;
            while cursor < bytes.len() {
                if bytes[cursor] == b'\\' {
                    return Err(crate::i18n::current().debug_explain_backslash());
                }
                if bytes[cursor] == quote {
                    cursor += 1;
                    if bytes.get(cursor) == Some(&quote) {
                        cursor += 1;
                    } else {
                        closed = true;
                        break;
                    }
                } else {
                    cursor += 1;
                }
            }
            if !closed {
                return Err(crate::i18n::current().debug_explain_quote());
            }
            tokens.push(if quote == b'\'' {
                Token::Other
            } else {
                Token::Identifier
            });
            continue;
        }
        if byte == b'[' || byte == b'\\' || byte == b'\0' {
            return Err(crate::i18n::current().debug_explain_lexical());
        }
        if byte == b'$' || byte == b'@' {
            return Err(crate::i18n::current().debug_explain_variables());
        }
        if byte.is_ascii_alphabetic() || byte == b'_' || !byte.is_ascii() {
            let start = cursor;
            cursor += 1;
            while bytes.get(cursor).is_some_and(|byte| {
                byte.is_ascii_alphanumeric() || *byte == b'_' || !byte.is_ascii()
            }) {
                cursor += 1;
            }
            tokens.push(Token::Word(sql[start..cursor].to_ascii_uppercase()));
            continue;
        }
        tokens.push(match byte {
            b'(' => Token::Open,
            b'.' => Token::Dot,
            _ => Token::Other,
        });
        cursor += 1;
    }
    if !matches!(tokens.first(), Some(Token::Word(word)) if word == "SELECT") {
        return Err(crate::i18n::current().debug_explain_select_only());
    }
    for (position, token) in tokens.iter().enumerate() {
        if let Token::Word(word) = token {
            if matches!(tokens.get(position + 1), Some(Token::Open))
                && matches!(
                    position.checked_sub(1).and_then(|index| tokens.get(index)),
                    Some(Token::Dot)
                )
            {
                return Err(crate::i18n::current().debug_explain_schema_function());
            }
            if matches!(
                word.as_str(),
                "WITH"
                    | "INSERT"
                    | "DELETE"
                    | "MERGE"
                    | "REPLACE"
                    | "INTO"
                    | "CREATE"
                    | "ALTER"
                    | "DROP"
                    | "TRUNCATE"
                    | "GRANT"
                    | "REVOKE"
                    | "CALL"
                    | "DO"
                    | "COPY"
                    | "EXPLAIN"
                    | "ANALYZE"
                    | "PRAGMA"
                    | "SET"
                    | "RESET"
                    | "ATTACH"
                    | "DETACH"
                    | "VACUUM"
                    | "LOAD"
                    | "EXECUTE"
                    | "PREPARE"
                    | "DEALLOCATE"
            ) {
                return Err(crate::i18n::current().debug_explain_subquery_write());
            }
            if word == "UPDATE"
                && !matches!(position.checked_sub(1).and_then(|index| tokens.get(index)), Some(Token::Word(previous)) if previous == "FOR")
            {
                return Err(crate::i18n::current().debug_explain_subquery_read_only());
            }
            if matches!(tokens.get(position + 1), Some(Token::Open))
                && !(matches!(word.as_str(), "INDEX" | "KEY")
                    && matches!(
                        position.checked_sub(1).and_then(|index| tokens.get(index)),
                        Some(Token::Word(previous)) if matches!(previous.as_str(), "USE" | "FORCE" | "IGNORE")
                    ))
                && !matches!(
                    word.as_str(),
                    "COUNT"
                        | "SUM"
                        | "MIN"
                        | "MAX"
                        | "AVG"
                        | "COALESCE"
                        | "NULLIF"
                        | "CAST"
                        | "LOWER"
                        | "UPPER"
                        | "LENGTH"
                        | "CHAR_LENGTH"
                        | "TRIM"
                        | "LTRIM"
                        | "RTRIM"
                        | "ABS"
                        | "ROUND"
                        | "SUBSTR"
                        | "SUBSTRING"
                        | "HEX"
                        | "IIF"
                        | "IN"
                        | "EXISTS"
                        | "NOT"
                        | "AND"
                        | "OR"
                        | "WHERE"
                        | "ON"
                        | "HAVING"
                        | "SELECT"
                        | "FROM"
                        | "JOIN"
                        | "AS"
                        | "BY"
                        | "OVER"
                        | "FILTER"
                        | "VALUES"
                        | "ONLY"
                )
            {
                return Err(crate::i18n::current().debug_explain_function());
            }
        } else if matches!(token, Token::Identifier)
            && matches!(tokens.get(position + 1), Some(Token::Open))
        {
            return Err(crate::i18n::current().debug_explain_quoted_function());
        }
    }
    Ok(())
}

async fn explain_one(database: &Database, record: &QueryRecord, budget: Duration) -> IndexUsage {
    let started = Instant::now();
    let timeout_reason = if budget < PER_QUERY_BUDGET {
        crate::i18n::current().debug_explain_remaining_spent()
    } else {
        crate::i18n::current().debug_explain_timeout()
    };
    if record.backend != database.backend() {
        return IndexUsage::unavailable(crate::i18n::current().debug_explain_other_database());
    }
    if database.backend() == Backend::Sqlite {
        return explain_sqlite(database, record, budget, timeout_reason).await;
    }
    let mut connection = match tokio::time::timeout(budget, database.pool.acquire()).await {
        Ok(Ok(connection)) => connection,
        Ok(Err(error)) => {
            return IndexUsage::unavailable(
                &crate::i18n::current().debug_explain_connection(error.to_string()),
            );
        }
        Err(_) => return IndexUsage::unavailable(timeout_reason),
    };
    let statement = format!("EXPLAIN FORMAT=TRADITIONAL {}", record.sql);
    let mut query = sqlx::query(&statement).persistent(false);
    for parameter in &record.parameters {
        query = match parameter {
            SqlValue::Null => query.bind(Option::<String>::None),
            SqlValue::Bool(value) => query.bind(*value),
            SqlValue::Integer(value) => query.bind(*value),
            SqlValue::Unsigned(value) => query.bind(*value),
            SqlValue::Float(value) => query.bind(*value),
            SqlValue::Text(value) => query.bind(value),
            SqlValue::Bytes(value) => query.bind(value),
        };
    }
    let Some(mysql) = connection.as_mysql() else {
        return IndexUsage::unavailable(crate::i18n::current().debug_explain_mysql_only());
    };
    let result = tokio::time::timeout(
        budget.saturating_sub(started.elapsed()),
        query.fetch_all(mysql),
    )
    .await;
    match result {
        Ok(Ok(rows)) => mysql_plan(&rows).unwrap_or_else(|error| {
            IndexUsage::unavailable(&crate::i18n::current().debug_explain_output(error.to_string()))
        }),
        Ok(Err(error)) => {
            IndexUsage::unavailable(&crate::i18n::current().debug_explain_error(error.to_string()))
        }
        Err(_) => {
            // Do not give a connection with an unfinished diagnostic back to application code.
            connection.close_on_drop();
            IndexUsage::unavailable(timeout_reason)
        }
    }
}

/// SQLite uses the read pool: the connection has `query_only` on, so diagnostics take no write connection and cannot
/// write to the database. The SQL in the record is the statement as rewritten by the rewriter and actually executed.
async fn explain_sqlite(
    database: &Database,
    record: &QueryRecord,
    budget: Duration,
    timeout_reason: &str,
) -> IndexUsage {
    let started = Instant::now();
    let Some(pool) = database.pool.as_sqlite_read() else {
        return IndexUsage::unavailable(crate::i18n::current().debug_explain_sqlite_pool());
    };
    let statement = format!("EXPLAIN QUERY PLAN {}", record.sql);
    let mut query = sqlx::query(&statement).persistent(false);
    for parameter in &record.parameters {
        query = match parameter {
            SqlValue::Null => query.bind(Option::<String>::None),
            SqlValue::Bool(value) => query.bind(*value),
            SqlValue::Integer(value) => query.bind(*value),
            SqlValue::Unsigned(value) => match i64::try_from(*value) {
                Ok(value) => query.bind(value),
                Err(_) => {
                    return IndexUsage::unavailable(
                        crate::i18n::current().debug_explain_sqlite_integer(),
                    );
                }
            },
            SqlValue::Float(value) => query.bind(*value),
            SqlValue::Text(value) => query.bind(value),
            SqlValue::Bytes(value) => query.bind(value),
        };
    }
    let mut connection = match tokio::time::timeout(budget, pool.acquire()).await {
        Ok(Ok(connection)) => connection,
        Ok(Err(error)) => {
            return IndexUsage::unavailable(
                &crate::i18n::current().debug_explain_connection(error.to_string()),
            );
        }
        Err(_) => return IndexUsage::unavailable(timeout_reason),
    };
    let result = tokio::time::timeout(
        budget.saturating_sub(started.elapsed()),
        query.fetch_all(&mut *connection),
    )
    .await;
    match result {
        Ok(Ok(rows)) => sqlite_plan(&rows).unwrap_or_else(|error| {
            IndexUsage::unavailable(&crate::i18n::current().debug_explain_output(error.to_string()))
        }),
        Ok(Err(error)) => IndexUsage::unavailable(
            &crate::i18n::current().debug_explain_query_plan_error(error.to_string()),
        ),
        Err(_) => {
            connection.close_on_drop();
            IndexUsage::unavailable(timeout_reason)
        }
    }
}

fn sqlite_plan(rows: &[SqliteRow]) -> anyhow::Result<IndexUsage> {
    let details = rows
        .iter()
        .map(|row| row.try_get::<String, _>("detail"))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(sqlite_details(&details))
}

/// Classifies `detail` line by line: `SEARCH` is an index lookup, `SCAN … USING [COVERING] INDEX` is an index scan, a
/// `SCAN` without `USING` is a full table scan, and an `AUTOMATIC` index is a temporary index built on every execution,
/// counted as a full table scan. Scans of subqueries and CTEs themselves and the structure lines of compound queries
/// and multi-index OR are not table accesses; temporary B-trees (MySQL's filesort, roughly) are appended to the
/// summary. Lines it cannot recognize are reported honestly as "cannot analyze".
fn sqlite_details(details: &[String]) -> IndexUsage {
    if details.is_empty() {
        return IndexUsage::unavailable(crate::i18n::current().debug_explain_sqlite_empty());
    }
    let mut accesses = Vec::new();
    let mut derived = Vec::new();
    let mut temp_trees = Vec::new();
    for detail in details {
        let detail = detail.as_str();
        if let Some(name) = detail
            .strip_prefix("CO-ROUTINE ")
            .or_else(|| detail.strip_prefix("MATERIALIZE "))
        {
            derived.push(name.split(' ').next().unwrap_or(name));
            continue;
        }
        if let Some(purpose) = detail.strip_prefix("USE TEMP B-TREE FOR ") {
            temp_trees.push(purpose);
            continue;
        }
        let (search, rest) = if let Some(rest) = detail.strip_prefix("SEARCH ") {
            (true, rest)
        } else if let Some(rest) = detail.strip_prefix("SCAN ") {
            (false, rest)
        } else if [
            "COMPOUND QUERY",
            "LEFT-MOST SUBQUERY",
            "UNION",
            "EXCEPT",
            "INTERSECT",
            "MERGE",
            "MULTI-INDEX OR",
            "INDEX ",
            "LIST SUBQUERY",
            "SCALAR SUBQUERY",
            "CORRELATED ",
            "CREATE BLOOM FILTER",
            "BLOOM FILTER ON ",
            "RIGHT-JOIN ",
        ]
        .iter()
        .any(|prefix| detail.starts_with(prefix))
        {
            continue;
        } else {
            return IndexUsage::unavailable(
                &crate::i18n::current().debug_explain_sqlite_row(detail),
            );
        };
        if rest == "CONSTANT ROW" {
            continue;
        }
        let (table, using) = match rest.split_once(" USING ") {
            Some((table, using)) => (table, Some(using)),
            None if !rest.contains(' ') => (rest, None),
            None => {
                return IndexUsage::unavailable(
                    &crate::i18n::current().debug_explain_sqlite_row(detail),
                );
            }
        };
        if table.starts_with('(') || derived.contains(&table) {
            continue;
        }
        let index = using.and_then(|using| {
            using
                .strip_prefix("COVERING INDEX ")
                .or_else(|| using.strip_prefix("INDEX "))
                .map(|index| index.split(' ').next().unwrap_or(index))
        });
        let access = match (search, using, index) {
            (_, Some(using), _) if using.starts_with("AUTOMATIC ") => Access {
                kind: AccessKind::Scan,
                description: crate::i18n::current().debug_explain_auto_index(table),
            },
            (_, None, _) if !search => Access {
                kind: AccessKind::Scan,
                description: crate::i18n::current().debug_explain_sqlite_scan(table),
            },
            (true, Some(using), None) if using.starts_with("INTEGER PRIMARY KEY") => Access {
                kind: AccessKind::Seek,
                description: crate::i18n::current().debug_explain_rowid(table),
            },
            (true, Some(using), None) if using.starts_with("PRIMARY KEY") => Access {
                kind: AccessKind::Seek,
                description: crate::i18n::current().debug_explain_index(table, "PRIMARY", "SEARCH"),
            },
            (true, _, Some(index)) => Access {
                kind: AccessKind::Seek,
                description: crate::i18n::current().debug_explain_index(table, index, "SEARCH"),
            },
            (false, _, Some(index)) => Access {
                kind: AccessKind::IndexScan,
                description: crate::i18n::current().debug_explain_index(table, index, "SCAN"),
            },
            _ => {
                return IndexUsage::unavailable(
                    &crate::i18n::current().debug_explain_sqlite_row(detail),
                );
            }
        };
        accesses.push(access);
    }
    let mut usage = summarize(accesses);
    if !temp_trees.is_empty() {
        usage.summary = crate::i18n::current().debug_explain_temp_trees(
            &usage.summary,
            temp_trees.join(crate::i18n::current().common_list_separator()),
        );
    }
    usage
}

#[derive(Clone, Copy, PartialEq)]
enum AccessKind {
    Seek,
    IndexScan,
    Scan,
}

struct Access {
    kind: AccessKind,
    description: String,
}

fn summarize(accesses: Vec<Access>) -> IndexUsage {
    if accesses.is_empty() {
        return IndexUsage {
            status: "none",
            summary: crate::i18n::current().debug_explain_no_table().into(),
        };
    }
    let indexed = accesses
        .iter()
        .any(|access| access.kind != AccessKind::Scan);
    let scanned = accesses
        .iter()
        .any(|access| access.kind == AccessKind::Scan);
    let status = match (indexed, scanned) {
        (true, true) => "mixed",
        (true, false) => "indexed",
        _ => "scan",
    };
    let mut descriptions = Vec::new();
    for access in accesses {
        let description = crate::i18n::current().debug_explain_access(
            match access.kind {
                AccessKind::Seek => crate::i18n::current().debug_explain_seek(),
                AccessKind::IndexScan => crate::i18n::current().debug_explain_index_scan(),
                AccessKind::Scan => crate::i18n::current().debug_explain_scan(),
            },
            access.description,
        );
        if !descriptions.contains(&description) {
            descriptions.push(description);
        }
    }
    IndexUsage {
        status,
        summary: descriptions.join(crate::i18n::current().debug_explain_separator()),
    }
}

fn mysql_plan(rows: &[MySqlRow]) -> anyhow::Result<IndexUsage> {
    let rows = rows
        .iter()
        .map(|row| {
            Ok(MySqlPlanRow {
                table: mysql_plan_text(row, "table")?,
                kind: mysql_plan_text(row, "type")?,
                key: mysql_plan_text(row, "key")?,
                extra: mysql_plan_text(row, "Extra")?,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()
        .with_context(|| {
            format!(
                "EXPLAIN columns: {:?}",
                rows.first().map(|row| row
                    .columns()
                    .iter()
                    .map(|column| column.name())
                    .collect::<Vec<_>>())
            )
        })?;
    Ok(mysql_details(&rows))
}

fn mysql_plan_text(row: &MySqlRow, name: &str) -> anyhow::Result<Option<String>> {
    // MySQL supplies EXPLAIN's columns at execute time. SQLx 0.8.6 refreshes the
    // columns but keeps the prepare-time name map, which can be empty for EXPLAIN.
    // Resolve the actual result metadata to an ordinal; the SQL remains prepared
    // with the original bound parameters. These four fields are nullable UTF-8 text.
    let ordinal = row
        .columns()
        .iter()
        .position(|column| column.name() == name)
        .with_context(|| format!("EXPLAIN result has no {name} column"))?;
    Ok(row.try_get_unchecked(ordinal)?)
}

struct MySqlPlanRow {
    table: Option<String>,
    kind: Option<String>,
    key: Option<String>,
    extra: Option<String>,
}

fn mysql_details(rows: &[MySqlPlanRow]) -> IndexUsage {
    let mut accesses = Vec::new();
    if rows.is_empty() {
        return IndexUsage::unavailable(crate::i18n::current().debug_explain_mysql_empty());
    }
    for row in rows {
        let table = row.table.as_deref();
        let kind = row.kind.as_deref().unwrap_or_default();
        if table.is_none() || kind.is_empty() {
            let extra = row.extra.as_deref().unwrap_or_default();
            if ![
                "No tables used",
                "Impossible",
                "Select tables optimized away",
                "Zero limit",
                "no matching row",
                "No matching",
            ]
            .iter()
            .any(|reason| extra.contains(reason))
            {
                return IndexUsage::unavailable(
                    &crate::i18n::current().debug_explain_mysql_extra(extra),
                );
            }
            continue;
        }
        let table = table.expect("checked table");
        if !matches!(
            kind,
            "system"
                | "const"
                | "eq_ref"
                | "ref"
                | "fulltext"
                | "ref_or_null"
                | "index_merge"
                | "unique_subquery"
                | "index_subquery"
                | "range"
                | "index"
                | "ALL"
        ) {
            return IndexUsage::unavailable(&crate::i18n::current().debug_explain_mysql_kind(kind));
        }
        let key = row.key.as_deref();
        if let Some(key) = key.filter(|key| !key.is_empty() && kind != "ALL") {
            accesses.push(Access {
                kind: if kind == "index" {
                    AccessKind::IndexScan
                } else {
                    AccessKind::Seek
                },
                description: single_line(
                    &crate::i18n::current().debug_explain_index(table, key, kind),
                ),
            });
        } else if !kind.is_empty() {
            accesses.push(Access {
                kind: AccessKind::Scan,
                description: single_line(
                    &crate::i18n::current().debug_explain_no_index(table, kind),
                ),
            });
        }
    }
    summarize(accesses)
}

fn single_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::debug::trace::RequestProfile;

    fn plan_row(table: &str, kind: &str, key: Option<&str>) -> MySqlPlanRow {
        MySqlPlanRow {
            table: Some(table.into()),
            kind: Some(kind.into()),
            key: key.map(String::from),
            extra: None,
        }
    }

    #[test]
    fn select_gate_does_not_treat_literals_as_statements() {
        for sql in [
            "SELECT 1",
            "SELECT 'a; DROP TABLE x; --' AS literal",
            "SELECT \"literal;DELETE\" FROM t;",
            "SELECT COUNT(*) FROM t WHERE a IN (1,2)",
            "SELECT a FROM t FOR UPDATE",
            "SELECT a FROM t FORCE INDEX (gonof_thread_latest) WHERE tid = ?",
            "SELECT a FROM t USE KEY (PRIMARY) WHERE tid = ?",
            "SELECT a FROM t IGNORE INDEX (gonof_thread_latest)",
            "SELECT a FROM t FORCE INDEX FOR ORDER BY (gonof_thread_latest)",
        ] {
            assert!(safe_select(sql).is_ok(), "{sql}");
        }
        for sql in [
            "SELECT 1; DELETE FROM t",
            "SELECT 1; -- tail",
            "/* leading */ SELECT 1",
            "SELECT 1 /*! UNION SELECT 2 */",
            "EXPLAIN ANALYZE SELECT 1",
            "WITH x AS (DELETE FROM t RETURNING *) SELECT * FROM x",
            "UPDATE t SET a=1",
            "SELECT * INTO new_table FROM t",
            "SELECT dangerous_function()",
            "SELECT custom.lower('value')",
            "SELECT \"dangerous_function\"()",
            "SELECT 'a\\'; DELETE FROM t;'",
            "SELECT @value := 1",
            "SELECT INDEX('value')",
            "SELECT KEY('value')",
        ] {
            assert!(safe_select(sql).is_err(), "{sql}");
        }
    }

    #[test]
    fn mysql_distinguishes_lookup_index_scan_and_table_scan() {
        let mixed = mysql_details(&[
            plan_row("member", "ref", Some("gonof_member_email")),
            plan_row("post", "ALL", None),
        ]);
        assert_eq!(mixed.status, "mixed");
        assert!(
            mixed
                .summary
                .contains("Index lookup: member → gonof_member_email (ref)")
        );
        assert!(
            mixed
                .summary
                .contains("Full scan: post (ALL, no index chosen)")
        );
        let indexed = mysql_details(&[plan_row("member", "index", Some("gonof_member_email"))]);
        assert_eq!(indexed.status, "indexed");
        assert!(indexed.summary.contains("Index scan"));
        let primary = mysql_details(&[plan_row("post", "const", Some("PRIMARY"))]);
        assert_eq!(primary.status, "indexed");
        assert!(primary.summary.contains("PRIMARY"));
        let merge = mysql_details(&[plan_row("post", "index_merge", Some("first,second"))]);
        assert!(merge.summary.contains("first,second"));
    }

    #[test]
    fn mysql_does_not_claim_unknown_or_unused_access_as_an_index() {
        assert_eq!(mysql_details(&[]).status, "unavailable");
        assert_eq!(
            mysql_details(&[plan_row("post", "unknown", Some("PRIMARY"))]).status,
            "unavailable"
        );
        for kind in ["ALL", "ref"] {
            assert_eq!(
                mysql_details(&[plan_row("post", kind, None)]).status,
                "scan"
            );
        }
        for extra in [
            "No tables used",
            "Impossible WHERE",
            "Select tables optimized away",
            "Zero limit",
        ] {
            let row = MySqlPlanRow {
                table: None,
                kind: None,
                key: None,
                extra: Some(extra.into()),
            };
            assert_eq!(mysql_details(&[row]).status, "none");
        }
        let unknown = MySqlPlanRow {
            table: None,
            kind: None,
            key: None,
            extra: Some("New optimizer behavior".into()),
        };
        assert_eq!(mysql_details(&[unknown]).status, "unavailable");
    }

    fn details(lines: &[&str]) -> IndexUsage {
        sqlite_details(
            &lines
                .iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn sqlite_plans_distinguish_search_index_scan_table_scan_and_temp_trees() {
        let primary = details(&["SEARCH x USING INTEGER PRIMARY KEY (rowid=?)"]);
        assert_eq!(primary.status, "indexed");
        assert_eq!(
            primary.summary,
            "Index lookup: x → integer primary key (SEARCH)"
        );
        let composite = details(&["SEARCH p USING COVERING INDEX sqlite_autoindex_p_1"]);
        assert_eq!(
            composite.summary,
            "Index lookup: p → sqlite_autoindex_p_1 (SEARCH)"
        );
        let covering = details(&["SCAN t USING COVERING INDEX t__fid"]);
        assert_eq!(covering.status, "indexed");
        assert_eq!(covering.summary, "Index scan: t → t__fid (SCAN)");
        let sorted = details(&["SCAN t", "USE TEMP B-TREE FOR ORDER BY"]);
        assert_eq!(sorted.status, "scan");
        assert_eq!(
            sorted.summary,
            "Full scan: t (SCAN, no index chosen); temporary B-tree: ORDER BY"
        );
        let automatic = details(&["SCAN t", "SEARCH p USING AUTOMATIC COVERING INDEX (c=?)"]);
        assert_eq!(automatic.status, "scan");
        assert!(
            automatic
                .summary
                .contains("p (automatic temporary index, built on every run)")
        );
        let derived = details(&[
            "CO-ROUTINE q",
            "SEARCH t USING INDEX t__fid (fid=?)",
            "USE TEMP B-TREE FOR GROUP BY",
            "SCAN q",
            "SEARCH p USING INDEX sqlite_autoindex_p_1 (a=?)",
        ]);
        assert_eq!(derived.status, "indexed");
        assert_eq!(
            derived.summary,
            "Index lookup: t → t__fid (SEARCH); Index lookup: p → sqlite_autoindex_p_1 (SEARCH); temporary B-tree: GROUP BY"
        );
        let structural = details(&[
            "MULTI-INDEX OR",
            "INDEX 1",
            "LIST SUBQUERY 1",
            "SCAN p USING COVERING INDEX sqlite_autoindex_p_1",
            "CREATE BLOOM FILTER",
            "SEARCH t USING INTEGER PRIMARY KEY (rowid=?)",
            "INDEX 2",
            "SCALAR SUBQUERY 2",
            "COMPOUND QUERY",
            "LEFT-MOST SUBQUERY",
            "UNION USING TEMP B-TREE",
        ]);
        assert_eq!(structural.status, "indexed");
        assert_eq!(details(&["SCAN CONSTANT ROW"]).status, "none");
        assert_eq!(details(&[]).status, "unavailable");
        assert_eq!(
            details(&["SCAN t VIRTUAL TABLE INDEX 1:"]).status,
            "unavailable"
        );
        assert_eq!(details(&["NEW PLAN SHAPE"]).status, "unavailable");
    }

    #[test]
    fn failed_cancelled_driver_and_unreliable_queries_are_never_explained() {
        let profile = Arc::new(RequestProfile::new());
        let mut complete = profile.start("SELECT ?");
        complete.parameters(Ok(vec![SqlValue::Unsigned(u64::MAX)]));
        complete.finish(None);
        profile
            .start("SELECT 1")
            .finish(Some("database error".into()));
        drop(profile.start("SELECT 2"));
        let mut unknown = profile.start("SELECT ?");
        unknown.parameters(Err("unavailable parameters".into()));
        unknown.finish(None);
        profile.record_driver(
            crate::db::Backend::MySql,
            "SET NAMES utf8mb4".into(),
            Duration::ZERO,
            0,
            Some(0),
        );
        let records = profile.snapshot();
        assert_eq!(skipped_reason(&records[0]), None);
        assert_eq!(
            skipped_reason(&records[1]),
            Some("The original query failed")
        );
        assert_eq!(
            skipped_reason(&records[2]),
            Some("The original query was cancelled")
        );
        assert_eq!(
            skipped_reason(&records[3]),
            Some("The parameters of the original query cannot be matched reliably")
        );
        assert_eq!(
            skipped_reason(&records[4]),
            Some("A driver batch has no separate statement parameters to rebind")
        );
    }

    #[test]
    fn diagnostic_limits_do_not_depend_on_display_expansion() {
        let profile = Arc::new(RequestProfile::new());
        let mut missing = profile.start("SELECT ?");
        missing.parameters(Ok(Vec::new()));
        missing.finish(None);
        profile
            .start(&format!("SELECT 1{}", " ".repeat(MAX_SQL_BYTES)))
            .finish(None);
        let mut oversized = profile.start("SELECT ?");
        oversized.parameters(Ok(vec![SqlValue::Bytes(vec![0; MAX_PARAMETER_BYTES + 1])]));
        oversized.finish(None);
        let records = profile.snapshot();
        assert_eq!(
            skipped_reason(&records[0]),
            Some("The parameters of the original query cannot be matched reliably")
        );
        assert_eq!(
            skipped_reason(&records[1]),
            Some("The SQL exceeds the 64 KiB diagnostic limit")
        );
        assert_eq!(
            skipped_reason(&records[2]),
            Some("The bound parameters exceed the 64 KiB diagnostic limit")
        );
    }
}
