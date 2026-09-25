pub mod mysql;
pub mod postgres;
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

use crate::models::Driver;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
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

fn hex(bytes: &[u8]) -> String {
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableStructure {
    pub columns: Vec<ColumnDetail>,
    pub indexes: Vec<IndexInfo>,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveRequest {
    pub namespace: String,
    pub table: String,
    pub updates: Vec<RowUpdate>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EditStatement {
    pub sql: String,
    pub params: Vec<EditValue>,
    pub display: String,
    pub table: String,
    pub label: String,
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
            check_matched(statement, result.rows_affected())?;
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
    fn schema_columns_sql(&self, namespace: &str) -> String;
    fn quote_ident(&self, ident: &str) -> String;
    fn use_namespace_sql(&self, namespace: &str) -> Option<String>;

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
    pub pool: Pool,
    pub query_conn: tokio::sync::Mutex<Option<Conn>>,
    pub backend_id: Option<i64>,
    pub cancel: AtomicBool,
    pub namespace: Mutex<String>,
    pub tunnel: Option<ssh::Tunnel>,
}

impl Session {
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
}

impl SessionStore {
    pub async fn get(&self, connection_id: &str) -> Result<Arc<Session>, String> {
        self.sessions
            .lock()
            .await
            .get(connection_id)
            .cloned()
            .ok_or_else(|| "This connection is not open. Reconnect and try again.".to_string())
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

pub fn describe_error(err: sqlx::Error) -> String {
    match err {
        sqlx::Error::Database(db) => db.message().to_string(),
        sqlx::Error::PoolTimedOut => "Timed out waiting for a database connection.".into(),
        sqlx::Error::Io(io) => format!("Could not reach the database server: {io}"),
        sqlx::Error::Tls(tls) => format!("TLS error: {tls}"),
        other => other.to_string(),
    }
}

pub async fn with_timeout<T>(
    future: impl std::future::Future<Output = Result<T, sqlx::Error>>,
) -> Result<T, String> {
    match tokio::time::timeout(CONNECT_TIMEOUT, future).await {
        Ok(result) => result.map_err(describe_error),
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

pub fn index_columns_from_definition(definition: &str) -> String {
    match (definition.find('('), definition.rfind(')')) {
        (Some(start), Some(end)) if end > start => definition[start + 1..end].to_string(),
        _ => definition.to_string(),
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
