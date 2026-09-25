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
