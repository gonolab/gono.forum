//! Decode schema text without conflating a binary collation with a binary payload.
use sqlx::{
    Column, ColumnIndex, Decode, Error, MySql, Row, Sqlite, Type, TypeInfo, ValueRef,
    mysql::MySqlRow, sqlite::SqliteRow,
};

/// Rust types that can be decoded from a column. Backend decoding bounds are written only here; callers' generic
/// functions write `T: for<'r> Decodable<'r>`.
pub trait Decodable<'r>:
    Decode<'r, MySql> + Type<MySql> + Decode<'r, Sqlite> + Type<Sqlite>
{
}
impl<'r, T> Decodable<'r> for T where
    T: Decode<'r, MySql> + Type<MySql> + Decode<'r, Sqlite> + Type<Sqlite>
{
}

/// What can locate a column: a column name (`&str`) or a 0-based index (`usize`).
pub trait ColumnName: ColumnIndex<MySqlRow> + ColumnIndex<SqliteRow> {}
impl<I: ColumnIndex<MySqlRow> + ColumnIndex<SqliteRow>> ColumnName for I {}

/// One row of a query result. Read only via `try_get` / `get` / [`super::text`]; the backend row type is not exposed.
pub struct DbRow(RowKind);

enum RowKind {
    MySql(MySqlRow),
    Sqlite(SqliteRow),
}

impl std::fmt::Debug for DbRow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            RowKind::MySql(row) => row.fmt(formatter),
            // SqliteRow does not implement Debug, so list the column names.
            RowKind::Sqlite(row) => formatter
                .debug_struct("SqliteRow")
                .field(
                    "columns",
                    &row.columns().iter().map(Column::name).collect::<Vec<_>>(),
                )
                .finish(),
        }
    }
}

impl From<MySqlRow> for DbRow {
    fn from(row: MySqlRow) -> Self {
        Self(RowKind::MySql(row))
    }
}

impl From<SqliteRow> for DbRow {
    fn from(row: SqliteRow) -> Self {
        Self(RowKind::Sqlite(row))
    }
}

impl DbRow {
    /// Only for MySQL-specific diagnostic code (reading EXPLAIN output column by column).
    pub fn as_mysql(&self) -> Option<&MySqlRow> {
        match &self.0 {
            RowKind::MySql(row) => Some(row),
            RowKind::Sqlite(_) => None,
        }
    }

    pub fn len(&self) -> usize {
        match &self.0 {
            RowKind::MySql(row) => row.len(),
            RowKind::Sqlite(row) => row.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The columns of the result set, in order.
    pub fn columns(&self) -> Vec<DbColumn<'_>> {
        match &self.0 {
            RowKind::MySql(row) => row
                .columns()
                .iter()
                .map(|column| DbColumn {
                    name: column.name(),
                    integer: matches!(
                        column.type_info().name(),
                        "BIGINT" | "INT" | "SMALLINT" | "TINYINT" | "MEDIUMINT"
                    ),
                })
                .collect(),
            // Table columns go by declared type (all integer columns are declared INTEGER in the baseline);
            // expressions go by this row's value.
            RowKind::Sqlite(row) => row
                .columns()
                .iter()
                .map(|column| DbColumn {
                    name: column.name(),
                    integer: column.type_info().name() == "INTEGER",
                })
                .collect(),
        }
    }

    pub fn try_get<'r, T, I>(&'r self, column: I) -> Result<T, Error>
    where
        I: ColumnName,
        T: Decodable<'r>,
    {
        match &self.0 {
            RowKind::MySql(row) => mysql_get(row, column),
            RowKind::Sqlite(row) => row.try_get(column),
        }
    }

    /// Whether this column is NULL, without decoding it as any type.
    pub fn is_null<I>(&self, column: I) -> Result<bool, Error>
    where
        I: ColumnName,
    {
        match &self.0 {
            RowKind::MySql(row) => Ok(row.try_get_raw(column)?.is_null()),
            RowKind::Sqlite(row) => Ok(row.try_get_raw(column)?.is_null()),
        }
    }

    pub fn get<'r, T, I>(&'r self, column: I) -> T
    where
        I: ColumnName,
        T: Decodable<'r>,
    {
        self.try_get(column).expect("database row decode")
    }

    /// Reads a column as text, ignoring the column metadata (see [`super::text`]).
    pub(super) fn text(&self, column: &str) -> Result<String, Error> {
        match &self.0 {
            RowKind::MySql(row) => row.try_get_unchecked(column),
            RowKind::Sqlite(row) => row.try_get_unchecked(column),
        }
    }
}

/// A column of the result set: its name, and whether it is an integer column (exporting JSON uses it to choose number
/// or string).
pub struct DbColumn<'r> {
    name: &'r str,
    integer: bool,
}

impl<'r> DbColumn<'r> {
    pub fn name(&self) -> &'r str {
        self.name
    }
    pub fn is_integer(&self) -> bool {
        self.integer
    }
}

fn mysql_get<'r, T, I>(row: &'r MySqlRow, column: I) -> Result<T, Error>
where
    I: ColumnIndex<MySqlRow>,
    T: Decode<'r, MySql> + Type<MySql>,
{
    // MySQL sets BINARY_FLAG for utf8mb4_bin VARCHAR/TEXT, although values are
    // UTF-8 text. SQLx's string type check rejects that flag. Only explicit text
    // decodes skip the metadata check; decoding still rejects invalid UTF-8 and
    // unexpected NULL, while numbers/bytes retain their normal type checks.
    let index = column.index(row)?;
    let actual = row.column(index).type_info().name();
    let binary_text = matches!(
        actual,
        "BINARY" | "VARBINARY" | "TINYBLOB" | "BLOB" | "MEDIUMBLOB" | "LONGBLOB"
    );
    if binary_text && T::type_info().name() == <String as Type<MySql>>::type_info().name() {
        row.try_get_unchecked(index)
    } else {
        row.try_get(index)
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        db::{Database, query, query_scalar, sql_display::SqlValue},
        debug::trace::{self, QuerySource, RequestProfile},
        test_support::TestFixture,
    };
    use std::sync::Arc;

    #[tokio::test]
    async fn cold_connection_initialization_is_driver_work_and_user_set_is_application_work() {
        // Only verifies MySQL session variables and connection initialisation.
        crate::mysql_only!();
        crate::debug::driver::install_for_tests();
        let fixture = TestFixture::migrated().await.unwrap();
        let cold = Database {
            pool: fixture.db.pool.reopen_lazy(0, 1),
        };
        let profile = Arc::new(RequestProfile::new());
        trace::scope(profile.clone(), async {
            assert_eq!(
                query_scalar::<i64>("SELECT ?")
                    .bind(42_i64)
                    .fetch_one(cold.executor())
                    .await
                    .unwrap(),
                42
            );
            query("SET @gonof_trace_probe = ?")
                .bind(7_i64)
                .execute(cold.executor())
                .await
                .unwrap();
        })
        .await;
        cold.pool.close().await;
        fixture.cleanup().await.unwrap();
        let records = profile.snapshot();
        let statements: Vec<_> = records
            .iter()
            .map(|record| (record.source, &record.sql))
            .collect();
        eprintln!("cold connection sources and placeholder SQL: {statements:?}");
        for (index, record) in records.iter().enumerate() {
            assert_eq!(record.sequence, index + 1);
            assert!(!record.cancelled);
            assert!(record.expansion_error.is_none());
        }
        let application: Vec<_> = records
            .iter()
            .filter(|record| record.source == QuerySource::Application)
            .collect();
        assert_eq!(application.len(), 2, "{statements:?}");
        assert_eq!(application[0].sql, "SELECT ?");
        assert_eq!(application[0].parameters, [SqlValue::Integer(42)]);
        assert_eq!(application[1].sql, "SET @gonof_trace_probe = ?");
        assert_eq!(application[1].parameters, [SqlValue::Integer(7)]);
        let driver: Vec<_> = records
            .iter()
            .filter(|record| record.source == QuerySource::Driver)
            .collect();
        assert!(
            driver.iter().any(|record| record.sql.contains("NAMES")),
            "{statements:?}"
        );
        assert!(
            driver
                .iter()
                .any(|record| record.sql.starts_with("SET SESSION sql_mode")),
            "{statements:?}"
        );
    }

    #[tokio::test]
    async fn binary_collation_text_scalar_null_and_bindings_decode_without_weakening_other_types() {
        // Only verifies the decoding workaround for utf8mb4_bin text that MySQL marks as BLOB. The text mixes three-,
        // two- and four-byte UTF-8: "\u{4e2d}\u{6587}" is the Chinese for "Chinese", "\u{7ed1}\u{5b9a}" for "binding".
        crate::mysql_only!();
        let fixture = TestFixture::migrated().await.unwrap();
        let result: anyhow::Result<()> = async {
            query("INSERT INTO gonof_common_settings(skey,svalue) VALUES(?,?)")
                .bind("row-fixture").bind("\u{4e2d}\u{6587} café 🍓").execute(fixture.db.executor()).await?;
            let row = query("SELECT skey,svalue,CAST(NULL AS CHAR CHARACTER SET utf8mb4) COLLATE utf8mb4_bin AS nullable_text, UNHEX('00FF') AS binary_value, CAST(-7 AS SIGNED) AS number, CAST(42 AS DECIMAL(12,2)) AS decimal_value FROM gonof_common_settings WHERE skey=?")
                .bind("row-fixture").fetch_one(fixture.db.executor()).await?;
            assert_eq!(row.try_get::<String,_>("skey")?, "row-fixture");
            assert_eq!(row.try_get::<String,_>("svalue")?, "\u{4e2d}\u{6587} café 🍓");
            assert_eq!(row.try_get::<Option<String>,_>("nullable_text")?, None);
            assert_eq!(row.try_get::<Vec<u8>,_>("binary_value")?, vec![0,255]);
            assert!(row.try_get::<String,_>("binary_value").is_err());
            assert!(row.try_get::<String,_>("number").is_err());
            assert!(row.try_get::<i64,_>("decimal_value").is_err());
            assert_eq!(row.try_get::<i64,_>("number")?, -7);
            assert_eq!(query_scalar::<String>("SELECT svalue FROM gonof_common_settings WHERE skey=?")
                .bind("row-fixture").fetch_one(fixture.db.executor()).await?, "\u{4e2d}\u{6587} café 🍓");
            assert_eq!(query_scalar::<Option<String>>("SELECT CAST(NULL AS CHAR CHARACTER SET utf8mb4) COLLATE utf8mb4_bin")
                .fetch_one(fixture.db.executor()).await?, None);
            let profile = Arc::new(RequestProfile::new());
            trace::scope(profile.clone(), async {
                assert_eq!(query_scalar::<String>("SELECT ?").bind("\u{7ed1}\u{5b9a} 🍓").fetch_one(fixture.db.executor()).await.unwrap(), "\u{7ed1}\u{5b9a} 🍓");
                assert_eq!(query_scalar::<u32>("SELECT ?").bind(u32::MAX).fetch_one(fixture.db.executor()).await.unwrap(), u32::MAX);
                assert_eq!(query_scalar::<Option<String>>("SELECT ?").bind(None::<String>).fetch_one(fixture.db.executor()).await.unwrap(), None);
            }).await;
            let records = profile.snapshot();
            for (index, record) in records.iter().enumerate() {
                assert_eq!(record.sequence, index + 1);
                assert!(!record.cancelled);
            }
            let records: Vec<_> = records.iter().filter(|record| record.source == QuerySource::Application).collect();
            assert_eq!(records.len(), 3);
            assert_eq!(records[0].parameters, vec![SqlValue::Text("\u{7ed1}\u{5b9a} 🍓".into())]);
            assert_eq!(records[1].parameters, vec![SqlValue::Unsigned(u64::from(u32::MAX))]);
            assert_eq!(records[2].parameters, vec![SqlValue::Null]);
            assert!(records.iter().all(|record|record.error.is_none() && record.expansion_error.is_none()));
            Ok(())
        }.await;
        fixture.cleanup().await.unwrap();
        result.unwrap();
    }
}
