pub mod dump;
pub mod mysql;
pub mod postgres;
pub mod sql_split;
pub mod sqlite;
pub mod ssh;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::TryStreamExt;
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize, Serializer};
use sqlx::{Column, Database, Either, Executor, Row, TypeInfo};

use crate::models::{ConnectionEntry, Driver};

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
pub const CONNECTION_LOST: &str = "The connection to the database server was lost";
pub const SESSION_LOST: &str = "The connection to the database server was lost. Reconnect to continue.";
const MAX_SAFE_JS_INT: i64 = 9_007_199_254_740_991;
const BYTES_PREVIEW: usize = 48;

#[derive(Debug, Clone, PartialEq)]
pub enum CellValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    Json(String),
    Bytes(Vec<u8>),
    DateTime(String),
}

impl CellValue {
    pub fn to_text(&self) -> Option<String> {
        match self {
            CellValue::Null => None,
            CellValue::Bool(value) => Some(value.to_string()),
            CellValue::Int(value) => Some(value.to_string()),
            CellValue::Float(value) => Some(value.to_string()),
            CellValue::Text(value) | CellValue::Json(value) | CellValue::DateTime(value) => {
                Some(value.clone())
            }
            CellValue::Bytes(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            CellValue::Int(value) => Some(*value),
            CellValue::Float(value) => Some(*value as i64),
            CellValue::Bool(value) => Some(i64::from(*value)),
            other => other.to_text()?.trim().parse().ok(),
        }
    }

    pub fn is_truthy(&self) -> bool {
        match self {
            CellValue::Bool(value) => *value,
            CellValue::Int(value) => *value != 0,
            other => matches!(
                other.to_text().as_deref().map(str::trim),
                Some("t" | "true" | "1" | "YES" | "yes")
            ),
        }
    }

    pub fn from_float(value: f64) -> Self {
        if value.is_finite() {
            CellValue::Float(value)
        } else {
            CellValue::Text(value.to_string())
        }
    }
}

pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

impl Serialize for CellValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            CellValue::Null => serializer.serialize_none(),
            CellValue::Bool(value) => serializer.serialize_bool(*value),
            CellValue::Int(value) if value.abs() <= MAX_SAFE_JS_INT => serializer.serialize_i64(*value),
            CellValue::Int(value) => serializer.serialize_str(&value.to_string()),
            CellValue::Float(value) => serializer.serialize_f64(*value),
            CellValue::Text(value) | CellValue::Json(value) | CellValue::DateTime(value) => {
                serializer.serialize_str(value)
            }
            CellValue::Bytes(bytes) => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("bytes", &bytes.len())?;
                map.serialize_entry("hex", &hex(&bytes[..bytes.len().min(BYTES_PREVIEW)]))?;
                map.end()
            }
        }
    }
}

pub type RowValues = Vec<CellValue>;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnMeta {
    pub name: String,
    pub type_name: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableInfo {
    pub name: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnDetail {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub default_value: Option<String>,
    pub primary_key: bool,
    pub extra: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexInfo {
    pub name: String,
    pub columns: String,
    pub unique: bool,
    pub primary: bool,
}

/** `columns[i]` references `ref_columns[i]`; composite keys have several. */
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForeignKey {
    pub name: String,
    pub columns: Vec<String>,
    pub ref_namespace: String,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableStructure {
    pub columns: Vec<ColumnDetail>,
    pub indexes: Vec<IndexInfo>,
    pub foreign_keys: Vec<ForeignKey>,
}

/** Rows arrive ordered by constraint, then by position within it. */
pub fn foreign_keys(output: &RawOutput) -> Vec<ForeignKey> {
    let mut keys: Vec<ForeignKey> = Vec::new();
    for row in output.text_rows() {
        let name = text_at(&row, 0);
        let (column, ref_column) = (text_at(&row, 1), text_at(&row, 4));
        if column.is_empty() || ref_column.is_empty() {
            continue;
        }
        match keys.last_mut() {
            Some(key) if key.name == name => {
                key.columns.push(column);
                key.ref_columns.push(ref_column);
            }
            _ => keys.push(ForeignKey {
                name,
                columns: vec![column],
                ref_namespace: text_at(&row, 2),
                ref_table: text_at(&row, 3),
                ref_columns: vec![ref_column],
            }),
        }
    }
    keys
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaColumn {
    pub table: String,
    pub column: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NamespaceList {
    pub items: Vec<String>,
    pub current: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortDirection {
    Asc,
    Desc,
}

impl SortDirection {
    pub fn sql(self) -> &'static str {
        match self {
            SortDirection::Asc => "ASC",
            SortDirection::Desc => "DESC",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowseRequest {
    pub namespace: String,
    pub table: String,
    pub offset: u64,
    pub limit: u64,
    pub order_by: Option<String>,
    pub order_dir: Option<SortDirection>,
    #[serde(default)]
    pub count: bool,
    /// Rows must match every column, as in `WHERE a = 1 AND b = 'x'`.
    #[serde(default)]
    pub filter: Vec<CellEdit>,
}

/** The WHERE clause for a browse filter, with values inlined as escaped literals. */
pub fn filter_sql(driver: Driver, filter: &[CellEdit]) -> String {
    if filter.is_empty() {
        return String::new();
    }
    let dialect = dialect(driver);
    let conditions: Vec<String> = filter
        .iter()
        .map(|cell| {
            let column = dialect.quote_ident(&cell.column);
            let value = match (&cell.value, driver) {
                (EditValue::Null, _) => return format!("{column} IS NULL"),
                (value, Driver::Postgres) => value.postgres_literal(),
                (EditValue::Bool(value), _) => i64::from(*value).to_string(),
                (EditValue::Int(value), _) => value.to_string(),
                (EditValue::Float(value), _) => value.to_string(),
                (EditValue::Text(text), Driver::Mysql) => quote_literal(&text.replace('\\', "\\\\")),
                (EditValue::Text(text), Driver::Sqlite) => quote_literal(text),
            };
            format!("{column} = {value}")
        })
        .collect();
    format!(" WHERE {}", conditions.join(" AND "))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowseResult {
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<RowValues>,
    pub offset: u64,
    pub total: Option<u64>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EditValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
}

impl<'de> Deserialize<'de> for EditValue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde_json::Value;
        match Value::deserialize(deserializer)? {
            Value::Null => Ok(EditValue::Null),
            Value::Bool(value) => Ok(EditValue::Bool(value)),
            Value::Number(number) => match number.as_i64() {
                Some(value) => Ok(EditValue::Int(value)),
                None => number
                    .as_f64()
                    .map(EditValue::Float)
                    .ok_or_else(|| serde::de::Error::custom("number out of range")),
            },
            Value::String(value) => Ok(EditValue::Text(value)),
            _ => Err(serde::de::Error::custom("expected null, a boolean, a number, or a string")),
        }
    }
}

impl EditValue {
    fn text(&self) -> Option<String> {
        match self {
            EditValue::Null => None,
            EditValue::Bool(value) => Some(value.to_string()),
            EditValue::Int(value) => Some(value.to_string()),
            EditValue::Float(value) => Some(value.to_string()),
            EditValue::Text(value) => Some(value.clone()),
        }
    }

    /// Postgres infers the column type from an untyped literal, so every value is quoted.
    fn postgres_literal(&self) -> String {
        match self.text() {
            None => "NULL".into(),
            Some(text) => format!("E'{}'", text.replace('\\', "\\\\").replace('\'', "''")),
        }
    }

    fn display_literal(&self) -> String {
        match self {
            EditValue::Null => "NULL".into(),
            EditValue::Bool(_) | EditValue::Int(_) | EditValue::Float(_) => {
                self.text().unwrap_or_default()
            }
            EditValue::Text(text) => quote_literal(text),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CellEdit {
    pub column: String,
    pub value: EditValue,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowUpdate {
    pub key: Vec<CellEdit>,
    pub changes: Vec<CellEdit>,
}

fn present<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(deserializer).map(Some)
}

/**
 * Only the fields that changed are set. `default_value` is `Some(None)` to
 * drop the default, and defaults are SQL expressions, not plain values.
 */
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnChange {
    pub column: String,
    pub name: Option<String>,
    pub data_type: Option<String>,
    pub nullable: Option<bool>,
    #[serde(default, deserialize_with = "present")]
    pub default_value: Option<Option<String>>,
}

impl ColumnChange {
    fn renamed(&self) -> Option<&str> {
        self.name.as_deref().filter(|name| *name != self.column)
    }

    fn only_renames(&self) -> bool {
        self.data_type.is_none() && self.nullable.is_none() && self.default_value.is_none()
    }
}

/** Columns left out of `values` get their default. */
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowInsert {
    pub values: Vec<CellEdit>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewColumn {
    pub name: String,
    pub data_type: String,
    #[serde(default = "yes")]
    pub nullable: bool,
    #[serde(default)]
    pub default_value: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewIndex {
    pub name: String,
    pub columns: String,
    #[serde(default)]
    pub unique: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveRequest {
    pub namespace: String,
    pub table: String,
    pub updates: Vec<RowUpdate>,
    #[serde(default)]
    pub inserts: Vec<RowInsert>,
    #[serde(default)]
    pub columns: Vec<ColumnChange>,
    #[serde(default)]
    pub new_columns: Vec<NewColumn>,
    #[serde(default)]
    pub indexes: Vec<IndexChange>,
    #[serde(default)]
    pub new_indexes: Vec<NewIndex>,
}

pub fn insert_statement(driver: Driver, table: &str, insert: &RowInsert) -> EditStatement {
    let dialect = dialect(driver);
    let mut params = Vec::new();
    let mut columns = Vec::new();
    let mut values = Vec::new();
    let mut display_values = Vec::new();
    for cell in &insert.values {
        columns.push(dialect.quote_ident(&cell.column));
        values.push(match &cell.value {
            EditValue::Null => "NULL".to_string(),
            value if driver == Driver::Postgres => value.postgres_literal(),
            value => {
                params.push(value.clone());
                "?".to_string()
            }
        });
        display_values.push(cell.value.display_literal());
    }
    let (sql, display) = if columns.is_empty() {
        let sql = match driver {
            Driver::Mysql => format!("INSERT INTO {table} () VALUES ()"),
            _ => format!("INSERT INTO {table} DEFAULT VALUES"),
        };
        (sql.clone(), sql)
    } else {
        let columns = columns.join(", ");
        (
            format!("INSERT INTO {table} ({columns}) VALUES ({})", values.join(", ")),
            format!("INSERT INTO {table} ({columns}) VALUES ({})", display_values.join(", ")),
        )
    };
    EditStatement {
        sql,
        params,
        display,
        table: table.to_string(),
        label: "a new row".into(),
        expect_one_row: true,
    }
}

pub fn add_column_statement(driver: Driver, table: &str, column: &NewColumn) -> Result<EditStatement, String> {
    let name = column.name.trim();
    if name.is_empty() {
        return Err(format!("A new column in {table} needs a name."));
    }
    let data_type = column.data_type.trim();
    if data_type.is_empty() {
        return Err(format!("New column {name} needs a type."));
    }
    let mut sql = format!("ALTER TABLE {table} ADD COLUMN {} {data_type}", dialect(driver).quote_ident(name));
    if !column.nullable {
        sql.push_str(" NOT NULL");
    }
    if let Some(expr) = column.default_value.as_deref().map(str::trim).filter(|expr| !expr.is_empty()) {
        sql.push_str(&format!(" DEFAULT {expr}"));
    }
    Ok(EditStatement::schema(table, name, sql))
}

/** `table` is qualified; SQLite needs the bare `table_name` after ON. */
pub fn create_index_statement(
    driver: Driver,
    namespace: &str,
    table: &str,
    table_name: &str,
    index: &NewIndex,
) -> Result<EditStatement, String> {
    let name = index.name.trim();
    if name.is_empty() {
        return Err(format!("A new index on {table} needs a name."));
    }
    let columns = index.columns.trim();
    if columns.is_empty() {
        return Err(format!("New index {name} needs at least one column."));
    }
    let dialect = dialect(driver);
    let unique = if index.unique { "UNIQUE " } else { "" };
    let sql = match driver {
        Driver::Mysql => format!("ALTER TABLE {table} ADD {unique}INDEX {} ({columns})", dialect.quote_ident(name)),
        Driver::Postgres => format!("CREATE {unique}INDEX {} ON {table} ({columns})", dialect.quote_ident(name)),
        Driver::Sqlite => format!(
            "CREATE {unique}INDEX {} ON {} ({columns})",
            dialect.qualified(namespace, name),
            dialect.quote_ident(table_name)
        ),
    };
    Ok(EditStatement::schema(table, name, sql))
}

#[derive(Debug, Clone, PartialEq)]
pub struct EditStatement {
    pub sql: String,
    pub params: Vec<EditValue>,
    pub display: String,
    pub table: String,
    pub label: String,
    pub expect_one_row: bool,
}

impl EditStatement {
    fn schema(table: &str, column: &str, sql: String) -> Self {
        EditStatement {
            display: sql.clone(),
            sql,
            params: Vec::new(),
            table: table.to_string(),
            label: column.to_string(),
            expect_one_row: false,
        }
    }
}

fn validate_column_change(change: &ColumnChange) -> Result<(), String> {
    if change.name.as_deref().is_some_and(|name| name.trim().is_empty()) {
        return Err(format!("Column {} needs a name.", change.column));
    }
    if change.data_type.as_deref().is_some_and(|value| value.trim().is_empty()) {
        return Err(format!("Column {} needs a type.", change.column));
    }
    if matches!(&change.default_value, Some(Some(value)) if value.trim().is_empty()) {
        return Err(format!(
            "The default for column {} is empty. Set it to NULL to remove it, or use '' for an empty string.",
            change.column
        ));
    }
    Ok(())
}

/**
 * MySQL needs the column's live definition because CHANGE COLUMN replaces
 * every attribute, not just the ones being edited.
 */
pub fn alter_statements(
    driver: Driver,
    table: &str,
    change: &ColumnChange,
    mysql_current: Option<&mysql::ColumnDefinition>,
) -> Result<Vec<EditStatement>, String> {
    validate_column_change(change)?;
    let quote = |ident: &str| dialect(driver).quote_ident(ident);
    let column = quote(&change.column);
    let statement = |sql: String| EditStatement::schema(table, &change.column, sql);
    match driver {
        Driver::Mysql => {
            let current = mysql_current.ok_or_else(|| {
                format!("Column {} no longer exists in {table}. Reload and try again.", change.column)
            })?;
            Ok(vec![statement(mysql::change_column_sql(table, change, current)?)])
        }
        Driver::Postgres => {
            let mut sql = Vec::new();
            if let Some(data_type) = &change.data_type {
                sql.push(format!(
                    "ALTER TABLE {table} ALTER COLUMN {column} TYPE {data_type} USING {column}::{data_type}"
                ));
            }
            if let Some(nullable) = change.nullable {
                let action = if nullable { "DROP" } else { "SET" };
                sql.push(format!("ALTER TABLE {table} ALTER COLUMN {column} {action} NOT NULL"));
            }
            match &change.default_value {
                Some(Some(expr)) => {
                    sql.push(format!("ALTER TABLE {table} ALTER COLUMN {column} SET DEFAULT {expr}"))
                }
                Some(None) => sql.push(format!("ALTER TABLE {table} ALTER COLUMN {column} DROP DEFAULT")),
                None => {}
            }
            if let Some(name) = change.renamed() {
                sql.push(format!("ALTER TABLE {table} RENAME COLUMN {column} TO {}", quote(name)));
            }
            Ok(sql.into_iter().map(statement).collect())
        }
        Driver::Sqlite => {
            if !change.only_renames() {
                return Err(format!(
                    "SQLite can only rename columns. Changing the type, nullability, or default of {} needs the table rebuilt, which Recon doesn't do yet.",
                    change.column
                ));
            }
            Ok(change
                .renamed()
                .map(|name| statement(format!("ALTER TABLE {table} RENAME COLUMN {column} TO {}", quote(name))))
                .into_iter()
                .collect())
        }
    }
}

pub fn update_statement(driver: Driver, table: &str, update: &RowUpdate) -> Result<EditStatement, String> {
    if update.key.is_empty() {
        return Err("Rows can only be updated by primary key.".into());
    }
    if update.changes.is_empty() {
        return Err("There is nothing to update.".into());
    }
    let dialect = dialect(driver);
    let inline = driver == Driver::Postgres;
    let mut params = Vec::new();
    let mut value_sql = |value: &EditValue| {
        if inline {
            value.postgres_literal()
        } else {
            params.push(value.clone());
            "?".to_string()
        }
    };
    let mut set = Vec::new();
    let mut set_display = Vec::new();
    for change in &update.changes {
        let column = dialect.quote_ident(&change.column);
        let sql = match change.value {
            EditValue::Null => "NULL".to_string(),
            ref value => value_sql(value),
        };
        set.push(format!("{column} = {sql}"));
        set_display.push(format!("{column} = {}", change.value.display_literal()));
    }
    let mut filter = Vec::new();
    let mut filter_display = Vec::new();
    let mut label = Vec::new();
    for key in &update.key {
        let column = dialect.quote_ident(&key.column);
        let (sql, display) = match key.value {
            EditValue::Null => (format!("{column} IS NULL"), format!("{column} IS NULL")),
            ref value => (
                format!("{column} = {}", value_sql(value)),
                format!("{column} = {}", value.display_literal()),
            ),
        };
        filter.push(sql);
        filter_display.push(display);
        label.push(format!("{} = {}", key.column, key.value.display_literal()));
    }
    Ok(EditStatement {
        sql: format!("UPDATE {table} SET {} WHERE {}", set.join(", "), filter.join(" AND ")),
        params,
        display: format!(
            "UPDATE {table} SET {} WHERE {}",
            set_display.join(", "),
            filter_display.join(" AND ")
        ),
        table: table.to_string(),
        label: label.join(", "),
        expect_one_row: true,
    })
}

fn check_matched(statement: &EditStatement, matched: u64) -> Result<(), String> {
    match matched {
        1 => Ok(()),
        0 => Err(format!(
            "The row in {} where {} was not found. It may have been changed or deleted since it was loaded. Nothing was saved.",
            statement.table, statement.label
        )),
        count => Err(format!(
            "The row in {} where {} matched {count} rows, so it cannot be updated safely. Nothing was saved.",
            statement.table, statement.label
        )),
    }
}

macro_rules! apply_in_transaction {
    ($pool:expr, $statements:expr) => {{
        let mut tx = $pool.begin().await.map_err(describe_error)?;
        for statement in $statements {
            let mut query = sqlx::query(&statement.sql);
            for param in &statement.params {
                query = match param {
                    EditValue::Null => query.bind(None::<String>),
                    EditValue::Bool(value) => query.bind(*value),
                    EditValue::Int(value) => query.bind(*value),
                    EditValue::Float(value) => query.bind(*value),
                    EditValue::Text(value) => query.bind(value.as_str()),
                };
            }
            let result = query.execute(&mut *tx).await.map_err(describe_error)?;
            if statement.expect_one_row {
                check_matched(statement, result.rows_affected())?;
            }
        }
        tx.commit().await.map_err(describe_error)
    }};
}

#[derive(Debug)]
pub struct RawOutput {
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<RowValues>,
    pub rows_affected: u64,
    pub truncated: bool,
}

impl RawOutput {
    pub fn text_rows(&self) -> Vec<Vec<Option<String>>> {
        self.rows.iter().map(|row| texts(row)).collect()
    }
}

pub fn first_text(output: &RawOutput) -> String {
    output
        .rows
        .first()
        .and_then(|row| row.first())
        .and_then(CellValue::to_text)
        .unwrap_or_default()
}

pub trait Dialect: Sync {
    fn version_sql(&self) -> &'static str;
    fn namespaces_sql(&self) -> &'static str;
    fn current_namespace_sql(&self) -> &'static str;
    fn namespace_label(&self) -> &'static str;
    fn system_namespaces(&self) -> &'static [&'static str];
    fn tables_sql(&self, namespace: &str) -> String;
    fn columns_sql(&self, namespace: &str, table: &str) -> String;
    fn indexes_sql(&self, namespace: &str, table: &str) -> String;
    /// Rows of name, columns, unique, primary, definition, index type, comment, constraint.
    fn index_definitions_sql(&self, namespace: &str, table: &str) -> String;
    fn schema_columns_sql(&self, namespace: &str) -> String;
    /// Rows of constraint, column, referenced namespace, table, and column, in key order.
    fn foreign_keys_sql(&self, namespace: &str, table: &str) -> String;
    fn quote_ident(&self, ident: &str) -> String;
    fn use_namespace_sql(&self, namespace: &str) -> Option<String>;
    fn create_namespace_sql(&self, namespace: &str) -> Option<String>;
    fn drop_namespace_sql(&self, namespace: &str) -> Option<String>;

    /// A single statement that renames the namespace, when the database has one.
    fn rename_namespace_sql(&self, _from: &str, _to: &str) -> Option<String> {
        None
    }

    fn index_columns(&self, raw: String) -> String {
        raw
    }

    fn qualified(&self, namespace: &str, table: &str) -> String {
        if namespace.is_empty() {
            return self.quote_ident(table);
        }
        format!("{}.{}", self.quote_ident(namespace), self.quote_ident(table))
    }
}

pub fn dialect(driver: Driver) -> &'static dyn Dialect {
    match driver {
        Driver::Mysql => &mysql::MysqlDialect,
        Driver::Postgres => &postgres::PostgresDialect,
        Driver::Sqlite => &sqlite::SqliteDialect,
    }
}

pub enum Pool {
    MySql(sqlx::MySqlPool),
    Postgres(sqlx::PgPool),
    Sqlite(sqlx::SqlitePool),
}

impl Pool {
    pub async fn close(&self) {
        match self {
            Pool::MySql(pool) => pool.close().await,
            Pool::Postgres(pool) => pool.close().await,
            Pool::Sqlite(pool) => pool.close().await,
        }
    }

    pub async fn run(&self, sql: &str, limit: usize) -> Result<RawOutput, String> {
        match self {
            Pool::MySql(pool) => {
                let mut conn = pool.acquire().await.map_err(describe_error)?;
                mysql::run(&mut conn, sql, limit, None).await
            }
            Pool::Postgres(pool) => {
                let mut conn = pool.acquire().await.map_err(describe_error)?;
                postgres::run(&mut conn, sql, limit, None).await
            }
            Pool::Sqlite(pool) => {
                let mut conn = pool.acquire().await.map_err(describe_error)?;
                sqlite::run(&mut conn, sql, limit, None).await
            }
        }
    }

    pub async fn apply(&self, statements: &[EditStatement]) -> Result<(), String> {
        match self {
            Pool::MySql(pool) => apply_in_transaction!(pool, statements),
            Pool::Postgres(pool) => apply_in_transaction!(pool, statements),
            Pool::Sqlite(pool) => apply_in_transaction!(pool, statements),
        }
    }

    /**
     * A connection taken out of the pool for good, so session settings and
     * open transactions from imports and exports never leak back into it.
     */
    pub async fn detached(&self) -> Result<Conn, String> {
        Ok(match self {
            Pool::MySql(pool) => Conn::MySql(pool.acquire().await.map_err(describe_error)?.detach()),
            Pool::Postgres(pool) => Conn::Postgres(pool.acquire().await.map_err(describe_error)?.detach()),
            Pool::Sqlite(pool) => Conn::Sqlite(pool.acquire().await.map_err(describe_error)?.detach()),
        })
    }
}

pub enum Conn {
    MySql(sqlx::MySqlConnection),
    Postgres(sqlx::PgConnection),
    Sqlite(sqlx::SqliteConnection),
}

impl Conn {
    pub async fn run(
        &mut self,
        sql: &str,
        limit: usize,
        cancel: Option<&AtomicBool>,
    ) -> Result<RawOutput, String> {
        match self {
            Conn::MySql(conn) => mysql::run(conn, sql, limit, cancel).await,
            Conn::Postgres(conn) => postgres::run(conn, sql, limit, cancel).await,
            Conn::Sqlite(conn) => sqlite::run(conn, sql, limit, cancel).await,
        }
    }

    /// Runs `sql` and discards any rows it returns.
    pub async fn execute(&mut self, sql: &str) -> Result<u64, String> {
        let query = sqlx::raw_sql(sql);
        match self {
            Conn::MySql(conn) => Executor::execute(conn, query).await.map(|done| done.rows_affected()),
            Conn::Postgres(conn) => Executor::execute(conn, query).await.map(|done| done.rows_affected()),
            Conn::Sqlite(conn) => Executor::execute(conn, query).await.map(|done| done.rows_affected()),
        }
        .map_err(describe_error)
    }

    pub async fn close(self) {
        use sqlx::Connection;
        let _ = match self {
            Conn::MySql(conn) => conn.close().await,
            Conn::Postgres(conn) => conn.close().await,
            Conn::Sqlite(conn) => conn.close().await,
        };
    }
}

pub struct Opened {
    pub pool: Pool,
    pub conn: Conn,
    pub backend_id: Option<i64>,
}

pub struct Session {
    pub name: String,
    pub driver: Driver,
    pub entry: ConnectionEntry,
    pub password: Option<String>,
    pub pool: Pool,
    pub query_conn: tokio::sync::Mutex<Option<Conn>>,
    pub backend_id: Option<i64>,
    pub cancel: AtomicBool,
    pub lost: AtomicBool,
    pub namespace: Mutex<String>,
    pub tunnel: Option<ssh::Tunnel>,
}

impl Session {
    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::Relaxed)
    }

    pub fn mark_lost(&self) {
        self.lost.store(true, Ordering::Relaxed);
    }

    pub fn namespace(&self) -> String {
        self.namespace
            .lock()
            .map(|value| value.clone())
            .unwrap_or_default()
    }

    pub fn set_namespace(&self, value: &str) {
        if let Ok(mut slot) = self.namespace.lock() {
            *slot = value.to_string();
        }
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    pub async fn close(&self) {
        if let Some(conn) = self.query_conn.lock().await.take() {
            conn.close().await;
        }
        self.pool.close().await;
        if let Some(tunnel) = &self.tunnel {
            tunnel.close().await;
        }
    }
}

#[derive(Default)]
pub struct SessionStore {
    sessions: tokio::sync::Mutex<HashMap<String, Arc<Session>>>,
    reconnecting: tokio::sync::Mutex<()>,
}

impl SessionStore {
    pub async fn get(&self, connection_id: &str) -> Result<Arc<Session>, String> {
        let session = self
            .current(connection_id)
            .await
            .ok_or_else(|| "This connection is not open. Reconnect and try again.".to_string())?;
        if session.is_lost() {
            return Err(SESSION_LOST.into());
        }
        Ok(session)
    }

    pub async fn current(&self, connection_id: &str) -> Option<Arc<Session>> {
        self.sessions.lock().await.get(connection_id).cloned()
    }

    /**
     * Held while a session is being rebuilt so concurrent failures on the
     * same dropped connection trigger a single reconnect.
     */
    pub async fn reconnect_guard(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.reconnecting.lock().await
    }

    /**
     * Swaps in a rebuilt session only if `stale` is still the current one,
     * so a reconnect never resurrects a connection the user has closed.
     */
    pub async fn replace(
        &self,
        connection_id: &str,
        stale: &Arc<Session>,
        next: Session,
    ) -> Result<Arc<Session>, Arc<Session>> {
        let next = Arc::new(next);
        let mut sessions = self.sessions.lock().await;
        match sessions.get(connection_id) {
            Some(current) if Arc::ptr_eq(current, stale) => {
                sessions.insert(connection_id.to_string(), next.clone());
                Ok(next)
            }
            _ => Err(next),
        }
    }

    pub async fn insert(&self, connection_id: &str, session: Session) -> Option<Arc<Session>> {
        self.sessions
            .lock()
            .await
            .insert(connection_id.to_string(), Arc::new(session))
    }

    pub async fn remove(&self, connection_id: &str) -> Option<Arc<Session>> {
        self.sessions.lock().await.remove(connection_id)
    }
}

struct StoredResult {
    connection_id: String,
    rows: Vec<RowValues>,
}

#[derive(Default)]
pub struct ResultStore {
    results: Mutex<HashMap<String, StoredResult>>,
}

impl ResultStore {
    pub fn insert(&self, connection_id: &str, rows: Vec<RowValues>) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        if let Ok(mut results) = self.results.lock() {
            results.insert(
                id.clone(),
                StoredResult {
                    connection_id: connection_id.to_string(),
                    rows,
                },
            );
        }
        id
    }

    pub fn window(&self, result_id: &str, offset: usize, limit: usize) -> Result<Vec<RowValues>, String> {
        let results = self.results.lock().map_err(|err| err.to_string())?;
        let stored = results
            .get(result_id)
            .ok_or_else(|| "That result is no longer available. Run the query again.".to_string())?;
        let start = offset.min(stored.rows.len());
        let end = offset.saturating_add(limit).min(stored.rows.len());
        Ok(stored.rows[start..end].to_vec())
    }

    pub fn remove(&self, result_ids: &[String]) {
        if let Ok(mut results) = self.results.lock() {
            for id in result_ids {
                results.remove(id);
            }
        }
    }

    pub fn remove_connection(&self, connection_id: &str) {
        if let Ok(mut results) = self.results.lock() {
            results.retain(|_, stored| stored.connection_id != connection_id);
        }
    }
}

fn column_meta<DB: Database>(columns: &[DB::Column]) -> Vec<ColumnMeta> {
    columns
        .iter()
        .map(|column| ColumnMeta {
            name: column.name().to_string(),
            type_name: column.type_info().name().to_string(),
        })
        .collect()
}

fn returns_rows(sql: &str) -> bool {
    let keyword = sql
        .trim_start_matches(|c: char| c.is_whitespace() || c == '(')
        .split(|c: char| !c.is_ascii_alphabetic())
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    matches!(
        keyword.as_str(),
        "SELECT" | "WITH" | "SHOW" | "PRAGMA" | "EXPLAIN" | "DESCRIBE" | "DESC" | "VALUES" | "TABLE"
    )
}

pub async fn run_raw<DB>(
    conn: &mut DB::Connection,
    sql: &str,
    limit: usize,
    cancel: Option<&AtomicBool>,
    cell: fn(&DB::Row, usize) -> CellValue,
    affected: fn(&DB::QueryResult) -> u64,
) -> Result<RawOutput, String>
where
    DB: Database,
    for<'e> &'e mut DB::Connection: Executor<'e, Database = DB>,
{
    let mut columns: Option<Vec<ColumnMeta>> = None;
    let mut rows = Vec::new();
    let mut rows_affected = 0;
    let mut truncated = false;
    {
        let mut stream = sqlx::raw_sql(sql).fetch_many(&mut *conn);
        while let Some(item) = stream.try_next().await.map_err(describe_error)? {
            if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                return Err("Query cancelled.".into());
            }
            match item {
                Either::Left(result) => rows_affected += affected(&result),
                Either::Right(row) => {
                    if columns.is_none() {
                        columns = Some(column_meta::<DB>(row.columns()));
                    }
                    if rows.len() >= limit {
                        truncated = true;
                        break;
                    }
                    rows.push((0..row.len()).map(|index| cell(&row, index)).collect());
                }
            }
        }
    }
    let columns = match columns {
        Some(columns) => columns,
        None if returns_rows(sql) => match (&mut *conn).describe(sql).await {
            Ok(described) => column_meta::<DB>(described.columns()),
            Err(_) => Vec::new(),
        },
        None => Vec::new(),
    };
    Ok(RawOutput {
        columns,
        rows,
        rows_affected,
        truncated,
    })
}

fn describe_plain(err: sqlx::Error) -> String {
    match err {
        sqlx::Error::Database(db) => db.message().to_string(),
        sqlx::Error::PoolTimedOut => "Timed out waiting for a database connection.".into(),
        sqlx::Error::Io(io) => format!("Could not reach the database server: {io}"),
        sqlx::Error::Tls(tls) => format!("TLS error: {tls}"),
        other => other.to_string(),
    }
}

/**
 * Errors that mean the connection itself is gone (network drop, server
 * restart, idle timeout, dead SSH tunnel) rather than the statement failing.
 */
fn is_lost_connection(err: &sqlx::Error) -> bool {
    match err {
        sqlx::Error::Io(_) | sqlx::Error::PoolClosed | sqlx::Error::WorkerCrashed => true,
        sqlx::Error::Database(db) => {
            if let Some(mysql) = db.try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>() {
                // ER_SERVER_SHUTDOWN, ER_CONNECTION_KILLED, ER_CLIENT_INTERACTION_TIMEOUT
                if matches!(mysql.number(), 1053 | 1927 | 4031) {
                    return true;
                }
            }
            db.code().is_some_and(|code| {
                code.starts_with("08") || matches!(code.as_ref(), "57P01" | "57P02" | "57P03" | "57P05")
            })
        }
        _ => false,
    }
}

pub fn is_connection_lost(message: &str) -> bool {
    message.starts_with(CONNECTION_LOST)
}

pub fn describe_error(err: sqlx::Error) -> String {
    if !is_lost_connection(&err) {
        return describe_plain(err);
    }
    let detail = match err {
        sqlx::Error::Io(io) => io.to_string(),
        other => describe_plain(other),
    };
    format!("{CONNECTION_LOST} ({detail}).")
}

pub async fn with_timeout<T>(
    future: impl std::future::Future<Output = Result<T, sqlx::Error>>,
) -> Result<T, String> {
    match tokio::time::timeout(CONNECT_TIMEOUT, future).await {
        Ok(result) => result.map_err(describe_plain),
        Err(_) => Err("Timed out connecting to the database server.".into()),
    }
}

pub fn quote_double(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

pub fn quote_backtick(ident: &str) -> String {
    format!("`{}`", ident.replace('`', "``"))
}

pub fn quote_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

pub fn texts(values: &[CellValue]) -> Vec<Option<String>> {
    values.iter().map(CellValue::to_text).collect()
}

pub fn text_at(values: &[Option<String>], index: usize) -> String {
    values.get(index).cloned().flatten().unwrap_or_default()
}

fn find_ignore_case(haystack: &str, needle: &str) -> Option<usize> {
    haystack.to_ascii_lowercase().find(&needle.to_ascii_lowercase())
}

/**
 * The byte range inside the parentheses of an index's column list, skipping
 * quoted text so a partial index's WHERE clause is not mistaken for it.
 */
fn index_column_span(definition: &str) -> Option<(usize, usize)> {
    let from = find_ignore_case(definition, " USING ")
        .or_else(|| find_ignore_case(definition, " ON "))
        .unwrap_or(0);
    let open = from + definition[from..].find('(')?;
    let mut depth = 0;
    let mut quote: Option<char> = None;
    for (offset, ch) in definition[open..].char_indices() {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"' | '`') => quote = Some(ch),
            (None, '(') => depth += 1,
            (None, ')') => {
                depth -= 1;
                if depth == 0 {
                    return Some((open + 1, open + offset));
                }
            }
            _ => {}
        }
    }
    None
}

pub fn index_columns_from_definition(definition: &str) -> String {
    match index_column_span(definition) {
        Some((start, end)) => definition[start..end].to_string(),
        None => definition.to_string(),
    }
}

/** Only the fields that changed are set. `columns` is the SQL column list. */
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexChange {
    pub index: String,
    pub name: Option<String>,
    pub columns: Option<String>,
    pub unique: Option<bool>,
}

impl IndexChange {
    fn only_renames(&self) -> bool {
        self.columns.is_none() && self.unique.is_none()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct IndexDefinition {
    pub name: String,
    pub columns: String,
    pub unique: bool,
    pub primary: bool,
    /// The CREATE INDEX statement, for Postgres and SQLite.
    pub definition: String,
    /// MySQL's INDEX_TYPE, such as BTREE or FULLTEXT.
    pub index_type: String,
    pub comment: String,
    /// Backed by a constraint, so it cannot be dropped with DROP INDEX.
    pub constraint: bool,
}

pub fn index_definitions(driver: Driver, output: &RawOutput) -> Vec<IndexDefinition> {
    let dialect = dialect(driver);
    output
        .rows
        .iter()
        .map(|row| {
            let text = texts(row);
            IndexDefinition {
                name: text_at(&text, 0),
                columns: dialect.index_columns(text_at(&text, 1)),
                unique: row.get(2).is_some_and(CellValue::is_truthy),
                primary: row.get(3).is_some_and(CellValue::is_truthy),
                definition: text_at(&text, 4),
                index_type: text_at(&text, 5),
                comment: text_at(&text, 6),
                constraint: row.get(7).is_some_and(CellValue::is_truthy),
            }
        })
        .collect()
}

/** Swaps the name, uniqueness, and column list in a CREATE INDEX statement, keeping the rest. */
fn rebuild_index_sql(current: &IndexDefinition, name_sql: &str, unique: bool, columns: Option<&str>) -> Result<String, String> {
    let definition = current.definition.as_str();
    let on = find_ignore_case(definition, " ON ");
    let (Some(on), Some((start, end))) = (on, index_column_span(definition)) else {
        return Err(format!("Recon couldn't read the definition of index {}. Change it with SQL instead.", current.name));
    };
    let columns = columns.unwrap_or(&definition[start..end]);
    Ok(format!(
        "CREATE {}INDEX {name_sql}{}{}{}",
        if unique { "UNIQUE " } else { "" },
        &definition[on..start],
        columns.trim(),
        &definition[end..]
    ))
}

pub fn index_statements(
    driver: Driver,
    namespace: &str,
    table: &str,
    change: &IndexChange,
    current: Option<&IndexDefinition>,
) -> Result<Vec<EditStatement>, String> {
    if change.name.as_deref().is_some_and(|name| name.trim().is_empty()) {
        return Err(format!("Index {} needs a name.", change.index));
    }
    if change.columns.as_deref().is_some_and(|columns| columns.trim().is_empty()) {
        return Err(format!("Index {} needs at least one column.", change.index));
    }
    let current = current
        .ok_or_else(|| format!("Index {} no longer exists on {table}. Reload and try again.", change.index))?;
    if current.primary {
        return Err(format!("{} is the primary key, which Recon can't change yet.", change.index));
    }
    let dialect = dialect(driver);
    let statement = |sql: String| EditStatement::schema(table, &change.index, sql);
    let old = dialect.quote_ident(&current.name);
    let new_name = change.name.as_deref().unwrap_or(&current.name);
    let new = dialect.quote_ident(new_name);
    let unique = change.unique.unwrap_or(current.unique);
    match driver {
        Driver::Mysql => {
            if change.only_renames() {
                return Ok(vec![statement(format!("ALTER TABLE {table} RENAME INDEX {old} TO {new}"))]);
            }
            let columns = change.columns.as_deref().unwrap_or(&current.columns).trim();
            if columns.is_empty() {
                return Err(format!("Index {} uses expressions, so change it with SQL instead.", current.name));
            }
            let kind = match current.index_type.to_ascii_uppercase().as_str() {
                _ if unique => "UNIQUE INDEX",
                "FULLTEXT" => "FULLTEXT INDEX",
                "SPATIAL" => "SPATIAL INDEX",
                _ => "INDEX",
            };
            let comment = if current.comment.is_empty() {
                String::new()
            } else {
                format!(" COMMENT {}", quote_literal(&current.comment.replace('\\', "\\\\")))
            };
            Ok(vec![statement(format!(
                "ALTER TABLE {table} DROP INDEX {old}, ADD {kind} {new} ({columns}){comment}"
            ))])
        }
        Driver::Postgres => {
            let qualified_old = dialect.qualified(namespace, &current.name);
            if change.only_renames() {
                return Ok(vec![statement(format!("ALTER INDEX {qualified_old} RENAME TO {new}"))]);
            }
            if current.constraint {
                return Err(format!(
                    "Index {} belongs to a constraint, so Recon can only rename it. Change the constraint with SQL instead.",
                    current.name
                ));
            }
            let create = rebuild_index_sql(current, &new, unique, change.columns.as_deref())?;
            Ok(vec![statement(format!("DROP INDEX {qualified_old}")), statement(create)])
        }
        Driver::Sqlite => {
            if current.constraint || current.definition.is_empty() {
                return Err(format!(
                    "Index {} was created by a UNIQUE or PRIMARY KEY constraint, so SQLite can't change it.",
                    current.name
                ));
            }
            let create = rebuild_index_sql(current, &dialect.qualified(namespace, new_name), unique, change.columns.as_deref())?;
            Ok(vec![
                statement(format!("DROP INDEX {}", dialect.qualified(namespace, &current.name))),
                statement(create),
            ])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_cells_for_the_grid() {
        let row = vec![
            CellValue::Null,
            CellValue::Bool(true),
            CellValue::Int(42),
            CellValue::Int(i64::MAX),
            CellValue::Text("hi".into()),
            CellValue::Bytes(vec![0xde, 0xad]),
        ];
        let json = serde_json::to_string(&row).unwrap();
        assert_eq!(
            json,
            r#"[null,true,42,"9223372036854775807","hi",{"bytes":2,"hex":"dead"}]"#
        );
    }

    #[test]
    fn detects_row_returning_statements() {
        assert!(returns_rows("  select 1"));
        assert!(returns_rows("(SELECT 1) UNION (SELECT 2)"));
        assert!(returns_rows("WITH x AS (SELECT 1) SELECT * FROM x"));
        assert!(!returns_rows("UPDATE t SET a = 1"));
        assert!(!returns_rows("insert into t values (1)"));
    }

    #[test]
    fn flags_dropped_connections_but_not_statement_errors() {
        let broken = sqlx::Error::Io(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "broken pipe"));
        let message = describe_error(broken);
        assert!(is_connection_lost(&message), "{message}");
        assert!(message.contains("broken pipe"));
        assert!(is_connection_lost(&describe_error(sqlx::Error::PoolClosed)));
        assert!(!is_connection_lost(&describe_error(sqlx::Error::RowNotFound)));
        assert!(!is_connection_lost(&describe_error(sqlx::Error::PoolTimedOut)));
    }

    #[test]
    fn quotes_identifiers_and_literals() {
        assert_eq!(quote_double("we\"ird"), "\"we\"\"ird\"");
        assert_eq!(quote_backtick("we`ird"), "`we``ird`");
        assert_eq!(quote_literal("o'brien"), "'o''brien'");
    }

    #[test]
    fn extracts_index_columns() {
        assert_eq!(
            index_columns_from_definition("CREATE UNIQUE INDEX users_email ON public.users USING btree (email, lower(name))"),
            "email, lower(name)"
        );
    }

    fn sample_update() -> RowUpdate {
        serde_json::from_value(serde_json::json!({
            "key": [{ "column": "id", "value": 7 }],
            "changes": [
                { "column": "name", "value": "o'brien \\ co" },
                { "column": "note", "value": null },
                { "column": "active", "value": true },
            ],
        }))
        .unwrap()
    }

    #[test]
    fn binds_update_values_for_mysql_and_sqlite() {
        let statement = update_statement(Driver::Mysql, "`app`.`users`", &sample_update()).unwrap();
        assert_eq!(
            statement.sql,
            "UPDATE `app`.`users` SET `name` = ?, `note` = NULL, `active` = ? WHERE `id` = ?"
        );
        assert_eq!(
            statement.params,
            vec![
                EditValue::Text("o'brien \\ co".into()),
                EditValue::Bool(true),
                EditValue::Int(7),
            ]
        );
        assert_eq!(statement.label, "id = 7");
    }

    #[test]
    fn inlines_escaped_literals_for_postgres() {
        let statement = update_statement(Driver::Postgres, "\"users\"", &sample_update()).unwrap();
        assert_eq!(
            statement.sql,
            "UPDATE \"users\" SET \"name\" = E'o''brien \\\\ co', \"note\" = NULL, \"active\" = E'true' \
             WHERE \"id\" = E'7'"
        );
        assert!(statement.params.is_empty());
    }

    #[test]
    fn refuses_updates_without_a_key() {
        let mut update = sample_update();
        update.key.clear();
        assert!(update_statement(Driver::Sqlite, "\"users\"", &update).is_err());
    }

    fn column_change(value: serde_json::Value) -> ColumnChange {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn distinguishes_dropped_defaults_from_unchanged_ones() {
        let dropped = column_change(serde_json::json!({ "column": "a", "defaultValue": null }));
        assert_eq!(dropped.default_value, Some(None));
        let untouched = column_change(serde_json::json!({ "column": "a", "nullable": true }));
        assert_eq!(untouched.default_value, None);
    }

    #[test]
    fn alters_postgres_columns_and_renames_last() {
        let change = column_change(serde_json::json!({
            "column": "age",
            "name": "years",
            "dataType": "bigint",
            "nullable": false,
            "defaultValue": "0",
        }));
        let sql = alter_statements(Driver::Postgres, "\"people\"", &change, None)
            .unwrap()
            .into_iter()
            .map(|statement| statement.sql)
            .collect::<Vec<_>>();
        assert_eq!(
            sql,
            vec![
                "ALTER TABLE \"people\" ALTER COLUMN \"age\" TYPE bigint USING \"age\"::bigint",
                "ALTER TABLE \"people\" ALTER COLUMN \"age\" SET NOT NULL",
                "ALTER TABLE \"people\" ALTER COLUMN \"age\" SET DEFAULT 0",
                "ALTER TABLE \"people\" RENAME COLUMN \"age\" TO \"years\"",
            ]
        );
        let drop = column_change(serde_json::json!({ "column": "age", "defaultValue": null }));
        assert_eq!(
            alter_statements(Driver::Postgres, "\"people\"", &drop, None).unwrap()[0].sql,
            "ALTER TABLE \"people\" ALTER COLUMN \"age\" DROP DEFAULT"
        );
    }

    #[test]
    fn only_renames_sqlite_columns() {
        let rename = column_change(serde_json::json!({ "column": "a", "name": "b" }));
        let statements = alter_statements(Driver::Sqlite, "\"t\"", &rename, None).unwrap();
        assert_eq!(statements[0].sql, "ALTER TABLE \"t\" RENAME COLUMN \"a\" TO \"b\"");
        assert!(!statements[0].expect_one_row);
        let retype = column_change(serde_json::json!({ "column": "a", "dataType": "INTEGER" }));
        assert!(alter_statements(Driver::Sqlite, "\"t\"", &retype, None).is_err());
    }

    #[test]
    fn rejects_blank_names_types_and_defaults() {
        for value in [
            serde_json::json!({ "column": "a", "name": " " }),
            serde_json::json!({ "column": "a", "dataType": "" }),
            serde_json::json!({ "column": "a", "defaultValue": "" }),
        ] {
            assert!(alter_statements(Driver::Postgres, "\"t\"", &column_change(value), None).is_err());
        }
    }

    #[test]
    fn keeps_hidden_mysql_attributes_when_changing_a_column() {
        let current = mysql::ColumnDefinition {
            name: "title".into(),
            column_type: "varchar(191)".into(),
            nullable: true,
            default_value: Some("'draft'".into()),
            extra: "DEFAULT_GENERATED on update CURRENT_TIMESTAMP".into(),
            comment: "shown in 'lists'".into(),
            collation: Some("utf8mb4_unicode_ci".into()),
            generated: false,
        };
        let change = column_change(serde_json::json!({ "column": "title", "nullable": false }));
        let sql = alter_statements(Driver::Mysql, "`app`.`posts`", &change, Some(&current)).unwrap();
        assert_eq!(
            sql[0].sql,
            "ALTER TABLE `app`.`posts` CHANGE COLUMN `title` `title` varchar(191) COLLATE utf8mb4_unicode_ci \
             NOT NULL DEFAULT 'draft' on update CURRENT_TIMESTAMP COMMENT 'shown in ''lists'''"
        );

        let retype = column_change(serde_json::json!({
            "column": "title",
            "name": "headline",
            "dataType": "int",
            "defaultValue": null,
        }));
        let sql = alter_statements(Driver::Mysql, "`posts`", &retype, Some(&current)).unwrap();
        assert_eq!(
            sql[0].sql,
            "ALTER TABLE `posts` CHANGE COLUMN `title` `headline` int NULL on update CURRENT_TIMESTAMP \
             COMMENT 'shown in ''lists'''"
        );

        let generated = mysql::ColumnDefinition { generated: true, ..current };
        assert!(alter_statements(Driver::Mysql, "`posts`", &change, Some(&generated)).is_err());
        let rename = column_change(serde_json::json!({ "column": "title", "name": "t" }));
        assert_eq!(
            alter_statements(Driver::Mysql, "`posts`", &rename, Some(&generated)).unwrap()[0].sql,
            "ALTER TABLE `posts` RENAME COLUMN `title` TO `t`"
        );
        assert!(alter_statements(Driver::Mysql, "`posts`", &rename, None).is_err());
    }

    #[test]
    fn finds_index_columns_past_nested_parens_and_where_clauses() {
        assert_eq!(
            index_columns_from_definition(
                "CREATE INDEX active_email ON public.users USING btree (lower((email)::text)) WHERE (active AND name <> ')')"
            ),
            "lower((email)::text)"
        );
    }

    fn index(definition: &str, unique: bool, constraint: bool) -> IndexDefinition {
        IndexDefinition {
            name: "users_email".into(),
            columns: index_columns_from_definition(definition),
            unique,
            primary: false,
            definition: definition.into(),
            index_type: String::new(),
            comment: String::new(),
            constraint,
        }
    }

    fn index_change(value: serde_json::Value) -> IndexChange {
        serde_json::from_value(value).unwrap()
    }

    fn index_sql(driver: Driver, table: &str, change: &IndexChange, current: &IndexDefinition) -> Vec<String> {
        let namespace = if driver == Driver::Sqlite { "main" } else { "public" };
        index_statements(driver, namespace, table, change, Some(current))
            .unwrap()
            .into_iter()
            .map(|statement| statement.sql)
            .collect()
    }

    #[test]
    fn rebuilds_postgres_indexes_keeping_method_and_predicate() {
        let current = index(
            "CREATE INDEX users_email ON public.users USING btree (email) WHERE (active)",
            false,
            false,
        );
        let change = index_change(serde_json::json!({
            "index": "users_email",
            "name": "users_email_name",
            "columns": "email, name",
            "unique": true,
        }));
        assert_eq!(
            index_sql(Driver::Postgres, "\"public\".\"users\"", &change, &current),
            vec![
                "DROP INDEX \"public\".\"users_email\"",
                "CREATE UNIQUE INDEX \"users_email_name\" ON public.users USING btree (email, name) WHERE (active)",
            ]
        );
        let rename = index_change(serde_json::json!({ "index": "users_email", "name": "by_email" }));
        assert_eq!(
            index_sql(Driver::Postgres, "\"public\".\"users\"", &rename, &current),
            vec!["ALTER INDEX \"public\".\"users_email\" RENAME TO \"by_email\""]
        );
        let constrained = index(&current.definition, true, true);
        let retype = index_change(serde_json::json!({ "index": "users_email", "unique": false }));
        assert!(index_statements(Driver::Postgres, "public", "\"users\"", &retype, Some(&constrained)).is_err());
    }

    #[test]
    fn changes_mysql_indexes_in_one_statement() {
        let mut current = index("", false, false);
        current.columns = "title(20), created_at DESC".into();
        current.index_type = "FULLTEXT".into();
        current.comment = "for 'search'".into();
        let rename = index_change(serde_json::json!({ "index": "users_email", "name": "by_title" }));
        assert_eq!(
            index_sql(Driver::Mysql, "`posts`", &rename, &current),
            vec!["ALTER TABLE `posts` RENAME INDEX `users_email` TO `by_title`"]
        );
        let unique = index_change(serde_json::json!({ "index": "users_email", "unique": true }));
        assert_eq!(
            index_sql(Driver::Mysql, "`posts`", &unique, &current),
            vec![
                "ALTER TABLE `posts` DROP INDEX `users_email`, ADD UNIQUE INDEX `users_email` \
                 (title(20), created_at DESC) COMMENT 'for ''search'''"
            ]
        );
        let primary = IndexDefinition { primary: true, ..current };
        assert!(index_statements(Driver::Mysql, "", "`posts`", &rename, Some(&primary)).is_err());
    }

    #[tokio::test]
    async fn rebuilds_sqlite_indexes() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let pool = Pool::Sqlite(pool);
        for sql in [
            "CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT UNIQUE, name TEXT, active INT)",
            "create index users_name on users (name collate nocase) where active = 1",
        ] {
            pool.run(sql, 0).await.unwrap();
        }
        let sql = dialect(Driver::Sqlite).index_definitions_sql("main", "users");
        let current = index_definitions(Driver::Sqlite, &pool.run(&sql, 100).await.unwrap());
        let find = |name: &str| current.iter().find(|index| index.name == name).unwrap();
        assert!(find("sqlite_autoindex_users_1").constraint);
        let change = index_change(serde_json::json!({ "index": "users_name", "name": "by_name", "unique": true }));
        let statements = index_statements(Driver::Sqlite, "main", "\"users\"", &change, Some(find("users_name"))).unwrap();
        assert_eq!(
            statements[1].sql,
            "CREATE UNIQUE INDEX \"main\".\"by_name\" on users (name collate nocase) where active = 1"
        );
        pool.apply(&statements).await.unwrap();
        let after = index_definitions(Driver::Sqlite, &pool.run(&sql, 100).await.unwrap());
        let renamed = after.iter().find(|index| index.name == "by_name").unwrap();
        assert!(renamed.unique);
        assert!(!after.iter().any(|index| index.name == "users_name"));
        let auto = index_change(serde_json::json!({ "index": "sqlite_autoindex_users_1", "name": "x" }));
        assert!(index_statements(Driver::Sqlite, "main", "\"users\"", &auto, Some(find("sqlite_autoindex_users_1"))).is_err());
    }

    #[test]
    fn builds_inserts_for_each_driver() {
        let insert: RowInsert = serde_json::from_value(serde_json::json!({
            "values": [{ "column": "name", "value": "o'brien" }, { "column": "note", "value": null }],
        }))
        .unwrap();
        let mysql = insert_statement(Driver::Mysql, "`users`", &insert);
        assert_eq!(mysql.sql, "INSERT INTO `users` (`name`, `note`) VALUES (?, NULL)");
        assert_eq!(mysql.display, "INSERT INTO `users` (`name`, `note`) VALUES ('o''brien', NULL)");
        assert!(mysql.expect_one_row);
        let postgres = insert_statement(Driver::Postgres, "\"users\"", &insert);
        assert_eq!(postgres.sql, "INSERT INTO \"users\" (\"name\", \"note\") VALUES (E'o''brien', NULL)");
        let empty = RowInsert { values: Vec::new() };
        assert_eq!(insert_statement(Driver::Mysql, "`t`", &empty).sql, "INSERT INTO `t` () VALUES ()");
        assert_eq!(insert_statement(Driver::Sqlite, "\"t\"", &empty).sql, "INSERT INTO \"t\" DEFAULT VALUES");
    }

    #[test]
    fn adds_columns_and_indexes() {
        let column: NewColumn = serde_json::from_value(serde_json::json!({
            "name": "status", "dataType": "varchar(20)", "nullable": false, "defaultValue": "'new'",
        }))
        .unwrap();
        assert_eq!(
            add_column_statement(Driver::Mysql, "`posts`", &column).unwrap().sql,
            "ALTER TABLE `posts` ADD COLUMN `status` varchar(20) NOT NULL DEFAULT 'new'"
        );
        let bare: NewColumn = serde_json::from_value(serde_json::json!({ "name": "x", "dataType": "int" })).unwrap();
        assert_eq!(
            add_column_statement(Driver::Postgres, "\"t\"", &bare).unwrap().sql,
            "ALTER TABLE \"t\" ADD COLUMN \"x\" int"
        );
        let untyped: NewColumn = serde_json::from_value(serde_json::json!({ "name": "x", "dataType": " " })).unwrap();
        assert!(add_column_statement(Driver::Sqlite, "\"t\"", &untyped).is_err());

        let index = NewIndex { name: "by_status".into(), columns: "status, id".into(), unique: true };
        assert_eq!(
            create_index_statement(Driver::Mysql, "app", "`app`.`posts`", "posts", &index).unwrap().sql,
            "ALTER TABLE `app`.`posts` ADD UNIQUE INDEX `by_status` (status, id)"
        );
        assert_eq!(
            create_index_statement(Driver::Postgres, "public", "\"public\".\"posts\"", "posts", &index).unwrap().sql,
            "CREATE UNIQUE INDEX \"by_status\" ON \"public\".\"posts\" (status, id)"
        );
        assert_eq!(
            create_index_statement(Driver::Sqlite, "main", "\"main\".\"posts\"", "posts", &index).unwrap().sql,
            "CREATE UNIQUE INDEX \"main\".\"by_status\" ON \"posts\" (status, id)"
        );
    }

    #[tokio::test]
    async fn inserts_rows_and_adds_structure_in_sqlite() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let pool = Pool::Sqlite(pool);
        pool.run("CREATE TABLE posts (id INTEGER PRIMARY KEY, title TEXT)", 0).await.unwrap();
        let insert = RowInsert {
            values: vec![CellEdit { column: "title".into(), value: EditValue::Text("hello".into()) }],
        };
        let column = NewColumn {
            name: "status".into(),
            data_type: "TEXT".into(),
            nullable: false,
            default_value: Some("'draft'".into()),
        };
        let index = NewIndex { name: "by_status".into(), columns: "status".into(), unique: false };
        let statements = vec![
            insert_statement(Driver::Sqlite, "\"main\".\"posts\"", &insert),
            add_column_statement(Driver::Sqlite, "\"main\".\"posts\"", &column).unwrap(),
            create_index_statement(Driver::Sqlite, "main", "\"main\".\"posts\"", "posts", &index).unwrap(),
        ];
        pool.apply(&statements).await.unwrap();
        let saved = pool.run("SELECT title, status FROM posts", 10).await.unwrap();
        assert_eq!(saved.rows[0], vec![CellValue::Text("hello".into()), CellValue::Text("draft".into())]);
        let indexes = pool
            .run(&dialect(Driver::Sqlite).index_definitions_sql("main", "posts"), 10)
            .await
            .unwrap();
        assert_eq!(indexes.text_rows()[0][0].as_deref(), Some("by_status"));
    }

    #[tokio::test]
    async fn renames_sqlite_columns_alongside_row_updates() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let pool = Pool::Sqlite(pool);
        pool.run("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)", 0).await.unwrap();
        pool.run("INSERT INTO users VALUES (1, 'ada')", 0).await.unwrap();
        let update = RowUpdate {
            key: vec![CellEdit { column: "id".into(), value: EditValue::Int(1) }],
            changes: vec![CellEdit { column: "name".into(), value: EditValue::Text("grace".into()) }],
        };
        let mut statements = vec![update_statement(Driver::Sqlite, "\"users\"", &update).unwrap()];
        let rename = column_change(serde_json::json!({ "column": "name", "name": "full_name" }));
        statements.extend(alter_statements(Driver::Sqlite, "\"users\"", &rename, None).unwrap());
        pool.apply(&statements).await.unwrap();
        let saved = pool.run("SELECT full_name FROM users WHERE id = 1", 1).await.unwrap();
        assert_eq!(saved.rows[0][0], CellValue::Text("grace".into()));
    }

    #[tokio::test]
    async fn applies_updates_in_one_transaction() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let pool = Pool::Sqlite(pool);
        pool.run("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, score INTEGER)", 0)
            .await
            .unwrap();
        pool.run("INSERT INTO users VALUES (1, 'ada', 1), (2, 'bob', 2)", 0).await.unwrap();

        let update = |id: i64, name: &str| RowUpdate {
            key: vec![CellEdit { column: "id".into(), value: EditValue::Int(id) }],
            changes: vec![
                CellEdit { column: "name".into(), value: EditValue::Text(name.into()) },
                CellEdit { column: "score".into(), value: EditValue::Text("42".into()) },
            ],
        };
        let statements = [update(1, "ada'"), update(99, "ghost")]
            .iter()
            .map(|row| update_statement(Driver::Sqlite, "\"users\"", row).unwrap())
            .collect::<Vec<_>>();
        let error = pool.apply(&statements).await.unwrap_err();
        assert!(error.contains("id = 99"), "{error}");
        let unchanged = pool.run("SELECT name FROM users WHERE id = 1", 1).await.unwrap();
        assert_eq!(unchanged.rows[0][0], CellValue::Text("ada".into()));

        pool.apply(&statements[..1]).await.unwrap();
        let saved = pool.run("SELECT name, score FROM users WHERE id = 1", 1).await.unwrap();
        assert_eq!(saved.rows[0], vec![CellValue::Text("ada'".into()), CellValue::Int(42)]);
    }

    #[test]
    fn builds_browse_filters_for_each_driver() {
        let filter: Vec<CellEdit> = serde_json::from_value(serde_json::json!([
            { "column": "id", "value": 7 },
            { "column": "code", "value": "o'b\\c" },
            { "column": "gone", "value": null },
        ]))
        .unwrap();
        assert_eq!(
            filter_sql(Driver::Mysql, &filter),
            " WHERE `id` = 7 AND `code` = 'o''b\\\\c' AND `gone` IS NULL"
        );
        assert_eq!(
            filter_sql(Driver::Sqlite, &filter),
            " WHERE \"id\" = 7 AND \"code\" = 'o''b\\c' AND \"gone\" IS NULL"
        );
        assert_eq!(
            filter_sql(Driver::Postgres, &filter),
            " WHERE \"id\" = E'7' AND \"code\" = E'o''b\\\\c' AND \"gone\" IS NULL"
        );
        assert_eq!(filter_sql(Driver::Mysql, &[]), "");
    }

    #[tokio::test]
    async fn reads_sqlite_foreign_keys_and_follows_them() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let pool = Pool::Sqlite(pool);
        for sql in [
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)",
            "CREATE TABLE slots (day TEXT, hour INT, PRIMARY KEY (day, hour))",
            "CREATE TABLE posts (id INTEGER PRIMARY KEY, author INT REFERENCES users, \
             editor INT REFERENCES users(id), day TEXT, hour INT, \
             FOREIGN KEY (day, hour) REFERENCES slots (day, hour))",
            "INSERT INTO users VALUES (1, 'ada'), (2, 'bob')",
        ] {
            pool.run(sql, 0).await.unwrap();
        }
        let sql = dialect(Driver::Sqlite).foreign_keys_sql("main", "posts");
        let mut keys = foreign_keys(&pool.run(&sql, 100).await.unwrap());
        keys.sort_by(|a, b| a.columns.cmp(&b.columns));
        let summary: Vec<_> = keys
            .iter()
            .map(|key| (key.columns.join(","), key.ref_namespace.as_str(), key.ref_table.as_str(), key.ref_columns.join(",")))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("author".into(), "main", "users", "id".into()),
                ("day,hour".into(), "main", "slots", "day,hour".into()),
                ("editor".into(), "main", "users", "id".into()),
            ]
        );
        let filter = vec![CellEdit { column: "id".into(), value: EditValue::Int(2) }];
        let found = pool
            .run(&format!("SELECT name FROM \"main\".\"users\"{}", filter_sql(Driver::Sqlite, &filter)), 10)
            .await
            .unwrap();
        assert_eq!(found.rows, vec![vec![CellValue::Text("bob".into())]]);
    }

    #[test]
    fn result_store_windows_and_drops_by_connection() {
        let store = ResultStore::default();
        let rows = (0..10).map(|n| vec![CellValue::Int(n)]).collect();
        let id = store.insert("c1", rows);
        let window = store.window(&id, 8, 5).unwrap();
        assert_eq!(window, vec![vec![CellValue::Int(8)], vec![CellValue::Int(9)]]);
        store.remove_connection("c1");
        assert!(store.window(&id, 0, 1).is_err());
    }
}
