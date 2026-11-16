use std::borrow::Cow;

use anyhow::{Context, Result};
use sqlx::migrate::{Migration, MigrationType, Migrator};

use super::{Backend, Database, conn::BackendConn};

/// One registered migration. The file name must start with a four-digit version number; a unit test checks the
/// directory against the registry item by item.
///
/// Each version has a file of the same name in both `db/migrations/mysql/` and `db/migrations/sqlite/`, with the same
/// structure, each written in its own dialect; the schema on both sides after running all migrations is compared item
/// by item by `tests/sqlite_schema.rs`.
struct Source {
    version: i64,
    /// Only the unit test uses it to check the directory.
    #[cfg_attr(not(test), allow(dead_code))]
    file: &'static str,
    description: &'static str,
    mysql: &'static str,
    sqlite: &'static str,
}

impl Source {
    fn sql(&self, backend: Backend) -> &'static str {
        match backend {
            Backend::MySql => self.mysql,
            Backend::Sqlite => self.sqlite,
        }
    }
}

/// Registers migrations: `version => "file name", "description";`. The files in the two directories have the same
/// name.
macro_rules! sources {
    ($($version:literal => $file:literal, $description:literal;)+) => {
        &[$(Source {
            version: $version,
            file: $file,
            description: $description,
            mysql: include_str!(concat!("../../../../db/migrations/mysql/", $file)),
            sqlite: include_str!(concat!("../../../../db/migrations/sqlite/", $file)),
        }),+]
    };
}

/// Append only; never change a released entry: SQLx checksums the SQL text and refuses to start if one byte changes.
///
/// Number 1 is the baseline (82 tables and their seed rows). Later schema changes are appended from number 2 on.
const SOURCES: &[Source] = sources! {
    1 => "0001_baseline.sql", "forum baseline";
    2 => "0002_rank_board.sql", "leaderboard computation times";
    3 => "0003_skins.sql", "imported skins and the site's layout document";
};

/// The latest migration version. Tests read it when asserting "migrated to the latest", so adding a migration does not
/// require changing tests one by one.
pub const LATEST: i64 = SOURCES[SOURCES.len() - 1].version;

pub(super) async fn run(database: &Database, until: i64) -> Result<()> {
    let backend = database.backend();
    let migrator = Migrator {
        migrations: Cow::Owned(
            SOURCES
                .iter()
                .filter(|source| source.version <= until)
                .map(|source| {
                    Migration::new(
                        source.version,
                        Cow::Borrowed(source.description),
                        MigrationType::Simple,
                        Cow::Borrowed(source.sql(backend)),
                        false,
                    )
                })
                .collect(),
        ),
        ..Migrator::DEFAULT
    };
    let mut connection = database.pool.acquire().await?;
    // MySQL DDL implicitly commits. Close even on error/cancellation so the session
    // advisory lock is released; SQLx retains the dirty version for failed DDL.
    // On SQLite each migration runs in one transaction and is rolled back as a whole on failure.
    connection.close_on_drop();
    match connection.backend() {
        BackendConn::MySql(connection) => migrator.run_direct(connection).await,
        BackendConn::Sqlite(connection) => migrator.run_direct(connection).await,
    }
    .context(crate::i18n::current().setup_config_migrate())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn files(backend: &str) -> BTreeSet<String> {
        let mut files = BTreeSet::new();
        let directory = crate::project_root().join("db/migrations").join(backend);
        for entry in std::fs::read_dir(directory).unwrap() {
            let name = entry.unwrap().file_name().into_string().unwrap();
            assert!(
                name.ends_with(".sql"),
                "a migration directory holds only .sql files: {backend}/{name}"
            );
            files.insert(name);
        }
        files
    }

    #[test]
    fn every_migration_file_is_registered_in_order() {
        let registered: BTreeSet<_> = SOURCES.iter().map(|s| s.file.to_owned()).collect();
        assert_eq!(
            registered.len(),
            SOURCES.len(),
            "a file is registered twice"
        );
        for backend in ["mysql", "sqlite"] {
            let files = files(backend);
            let missing: Vec<_> = files.difference(&registered).collect();
            assert!(
                missing.is_empty(),
                "the {backend} directory has unregistered migrations: {missing:?}"
            );
            // A registered migration without a file fails to compile in include_str!; this only guards against the
            // two sides drifting apart after a file is renamed.
            assert_eq!(files, registered, "{backend}");
        }
        for (index, source) in SOURCES.iter().enumerate() {
            let expected = i64::try_from(index).unwrap() + 1;
            assert_eq!(
                source.version, expected,
                "versions must count up from 1 without gaps"
            );
            assert!(
                source.file.starts_with(&format!("{expected:04}_")),
                "the first four digits of the file name must be the version: {} is registered as {expected}",
                source.file
            );
            assert!(
                !source.description.trim().is_empty(),
                "{} has no description",
                source.file
            );
        }
        assert_eq!(LATEST, i64::try_from(SOURCES.len()).unwrap());
    }

    /// The project does not use triggers (docs/architecture.md, "Data"): no migration may create a trigger, and
    /// routines, events and `DEFINER` are not used either; `DROP TRIGGER` must not appear: the database has no
    /// triggers to begin with, and on 5.7 with binlog on it needs SUPER just like creating a trigger. A further reason:
    /// on a server with binlog on and `log_bin_trust_function_creators = 0`, an application account without SUPER
    /// fails these statements at startup with 1419; development instances usually have binlog off and do not show it,
    /// so it would only stop a deployment halfway through migrating.
    ///
    /// Applies to all migrations, with no exemptions. Only project migrations are scanned; triggers created
    /// temporarily in test code for fault injection are not covered.
    #[test]
    fn migrations_never_create_triggers_routines_or_events() {
        for source in SOURCES {
            for backend in [Backend::MySql, Backend::Sqlite] {
                let found = forbidden_statements(source.sql(backend));
                assert!(
                    found.is_empty(),
                    "{backend:?} {} contains a forbidden statement {found:?}: the project uses no triggers, routines or events (docs/architecture.md, \"Data\")",
                    source.file
                );
            }
        }
    }

    #[test]
    fn forbidden_statement_detector_reads_code_but_not_comments_or_literals() {
        for sql in [
            "CREATE TRIGGER t BEFORE INSERT ON x FOR EACH ROW SET NEW.a = 1",
            "create  definer = `root`@`localhost`\ntrigger t before update on x for each row set @a = 1",
            "CREATE FUNCTION f() RETURNS INT DETERMINISTIC RETURN 1",
            "CREATE AGGREGATE FUNCTION f RETURNS STRING SONAME 'x.so'",
            "CREATE PROCEDURE p() SELECT 1",
            "CREATE EVENT e ON SCHEDULE EVERY 1 DAY DO DELETE FROM x",
            "ALTER FUNCTION f COMMENT 'x'",
            "ALTER DEFINER = CURRENT_USER VIEW v AS SELECT 1",
            "/*!50003 CREATE*/ /*!50017 DEFINER=`root`@`localhost`*/ /*!50003 TRIGGER t AFTER INSERT ON x FOR EACH ROW SET @a = 1 */",
            "DROP TRIGGER IF EXISTS t",
            "ALTER TABLE x ADD COLUMN y INT; drop  trigger t;",
            "CREATE TEMP TRIGGER t AFTER INSERT ON x BEGIN SELECT RAISE(ABORT, 'x'); END",
            "/*!50003 DROP TRIGGER IF EXISTS t */;",
        ] {
            assert!(
                !forbidden_statements(sql).is_empty(),
                "must be caught: {sql}"
            );
        }
        for sql in [
            "-- CREATE TRIGGER is only a comment\nSELECT 1",
            "# CREATE FUNCTION\nSELECT 1",
            "/* DEFINER in an ordinary comment */ SELECT 1",
            "INSERT INTO x(note) VALUES ('CREATE TRIGGER'), (\"DEFINER\")",
            "CREATE TABLE x (`event` INT, `function` INT, trigger_name VARCHAR(10))",
            "ALTER TABLE x ADD COLUMN event_key VARCHAR(10), ADD INDEX trigger_idx (event_key)",
            "CREATE INDEX procedure_idx ON x (a)",
            "RENAME TABLE a TO b",
            "-- DROP TRIGGER t\nSELECT 1",
            "INSERT INTO x(note) VALUES ('DROP TRIGGER t')",
            "ALTER TABLE x DROP COLUMN trigger_name",
            "DROP TABLE IF EXISTS trigger_log",
        ] {
            assert!(
                forbidden_statements(sql).is_empty(),
                "must not be caught: {sql}"
            );
        }
    }

    /// Returns the kinds of forbidden statements in the SQL: creating triggers, routines and events, `DEFINER`, and
    /// `DROP TRIGGER` (all need SUPER with binlog on). Scans word by word after removing comments, strings and quoted
    /// identifiers; `/*! … */` versioned comments are executed by MySQL and are treated as code.
    fn forbidden_statements(sql: &str) -> Vec<&'static str> {
        const OBJECTS: &[&str] = &[
            "TABLE",
            "INDEX",
            "VIEW",
            "DATABASE",
            "SCHEMA",
            "USER",
            "ROLE",
            "SERVER",
            "TABLESPACE",
            "INSTANCE",
            "LOGFILE",
            "RESOURCE",
            "TRIGGER",
            "FUNCTION",
            "PROCEDURE",
            "EVENT",
        ];
        let words = code_words(sql);
        let mut found = Vec::new();
        for (index, word) in words.iter().enumerate() {
            if word == "DEFINER" && !found.contains(&"DEFINER") {
                found.push("DEFINER");
            }
            if word == "DROP"
                && words.get(index + 1).is_some_and(|next| next == "TRIGGER")
                && !found.contains(&"DROP TRIGGER")
            {
                found.push("DROP TRIGGER");
            }
            if word != "CREATE" && word != "ALTER" {
                continue;
            }
            // The first object keyword after CREATE/ALTER decides the statement kind; modifiers in between are skipped.
            let object = words[index + 1..]
                .iter()
                .find(|candidate| OBJECTS.contains(&candidate.as_str()));
            let kind = match (word.as_str(), object.map(String::as_str)) {
                ("CREATE", Some("TRIGGER")) => "CREATE TRIGGER",
                ("CREATE", Some("FUNCTION")) => "CREATE FUNCTION",
                ("CREATE", Some("PROCEDURE")) => "CREATE PROCEDURE",
                ("CREATE", Some("EVENT")) => "CREATE EVENT",
                ("ALTER", Some("FUNCTION")) => "ALTER FUNCTION",
                ("ALTER", Some("PROCEDURE")) => "ALTER PROCEDURE",
                ("ALTER", Some("EVENT")) => "ALTER EVENT",
                _ => continue,
            };
            if !found.contains(&kind) {
                found.push(kind);
            }
        }
        found
    }

    fn code_words(sql: &str) -> Vec<String> {
        code_text(sql)
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$'))
            .filter(|word| !word.is_empty())
            .map(str::to_ascii_uppercase)
            .collect()
    }

    /// Replaces comments, strings and quoted identifiers with spaces, keeping only code that would be executed; the
    /// contents of versioned comments are kept.
    fn code_text(sql: &str) -> String {
        let bytes = sql.as_bytes();
        let mut code = String::with_capacity(sql.len());
        let mut index = 0;
        let mut versioned = false;
        while index < bytes.len() {
            let rest = &bytes[index..];
            if rest.starts_with(b"/*!") {
                versioned = true;
                index += 3;
                while index < bytes.len() && bytes[index].is_ascii_digit() {
                    index += 1;
                }
                code.push(' ');
            } else if versioned && rest.starts_with(b"*/") {
                versioned = false;
                index += 2;
                code.push(' ');
            } else if rest.starts_with(b"/*") {
                index = find(bytes, index + 2, b"*/").map_or(bytes.len(), |end| end + 2);
                code.push(' ');
            } else if (rest.starts_with(b"--")
                && rest.get(2).is_none_or(|next| next.is_ascii_whitespace()))
                || rest[0] == b'#'
            {
                index = find(bytes, index, b"\n").unwrap_or(bytes.len());
                code.push(' ');
            } else if matches!(rest[0], b'\'' | b'"' | b'`') {
                index = skip_quoted(bytes, index);
                code.push(' ');
            } else {
                code.push(char::from(rest[0]));
                index += 1;
            }
        }
        code
    }

    fn find(bytes: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
        bytes[from..]
            .windows(needle.len())
            .position(|window| window == needle)
            .map(|offset| from + offset)
    }

    /// Skips a quoted section, supporting backslash escapes and doubled quotes. Returns the position after the closing
    /// quote.
    fn skip_quoted(bytes: &[u8], start: usize) -> usize {
        let quote = bytes[start];
        let mut index = start + 1;
        while index < bytes.len() {
            if bytes[index] == b'\\' && quote != b'`' {
                index += 2;
            } else if bytes[index] == quote {
                if bytes.get(index + 1) == Some(&quote) {
                    index += 2;
                } else {
                    return index + 1;
                }
            } else {
                index += 1;
            }
        }
        bytes.len()
    }
}
