use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use serde::Serialize;
use tauri::State;

use crate::commands::{sanitize_connection, AppState};
use crate::db::ssh::Tunnel;
use crate::db::{
    self, dialect, first_text, text_at, BrowseRequest, BrowseResult, CellValue, ColumnDetail,
    ColumnMeta, IndexInfo, NamespaceList, Pool, RawOutput, ResultStore, RowValues, SchemaColumn,
    Session, SessionStore, TableInfo, TableStructure,
};
use crate::models::{ConnectionEntry, Driver};
use crate::query_log::{self, QueryOrigin, QueryRecord};
use crate::secrets;

const FIRST_PAGE: usize = 200;
const BROWSE_LIMIT_MAX: u64 = 5_000;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub server_version: String,
    pub namespaces: NamespaceList,
    pub namespace_label: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatementResult {
    pub sql: String,
    pub result_id: Option<String>,
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<RowValues>,
    pub row_count: usize,
    pub truncated: bool,
    pub rows_affected: Option<u64>,
    pub duration_ms: u64,
    pub error: Option<String>,
}

fn resolve_password(entry: &ConnectionEntry, password: Option<String>) -> Result<Option<String>, String> {
    if let Some(password) = password.filter(|value| !value.is_empty()) {
        return Ok(Some(password));
    }
    if entry.driver == Driver::Sqlite || entry.id.is_empty() || !entry.save_password {
        return Ok(None);
    }
    secrets::get(&entry.id)
}

fn resolve_ssh_secret(entry: &ConnectionEntry, secret: Option<String>) -> Result<Option<String>, String> {
    if !entry.ssh.uses_secret() {
        return Ok(None);
    }
    if let Some(secret) = secret.filter(|value| !value.is_empty()) {
        return Ok(Some(secret));
    }
    if entry.id.is_empty() {
        return Ok(None);
    }
    secrets::get(&secrets::ssh_account(&entry.id))
}

async fn open_tunnel(
    entry: &ConnectionEntry,
    ssh_secret: Option<String>,
) -> Result<(ConnectionEntry, Option<Tunnel>), String> {
    if !entry.ssh.enabled || entry.driver == Driver::Sqlite {
        return Ok((entry.clone(), None));
    }
    let secret = resolve_ssh_secret(entry, ssh_secret)?;
    let tunnel = db::ssh::open(entry, secret.as_deref()).await?;
    Ok((tunnel.local_entry(entry), Some(tunnel)))
}

fn explain(tunnel: Option<&Tunnel>, err: String) -> String {
    match tunnel {
        Some(tunnel) => tunnel.explain(err),
        None => err,
    }
}

fn record(session: &Session, sql: &str, origin: QueryOrigin, started: Instant, outcome: &Result<RawOutput, String>) {
    let rows = outcome.as_ref().map(|output| {
        Some(if output.columns.is_empty() {
            output.rows_affected
        } else {
            output.rows.len() as u64
        })
    });
    let namespace = session.namespace();
    query_log::record(QueryRecord {
        connection: &session.name,
        driver: session.driver.label(),
        database: &namespace,
        sql,
        origin,
        duration: started.elapsed(),
        outcome: rows.map_err(|err| err.as_str()),
    });
}

async fn pool_run(session: &Session, sql: &str, limit: usize, origin: QueryOrigin) -> Result<RawOutput, String> {
    let started = Instant::now();
    let outcome = session.pool.run(sql, limit).await;
    record(session, sql, origin, started, &outcome);
    outcome
}

async fn conn_run(
    session: &Session,
    sql: &str,
    limit: usize,
    cancel: Option<&AtomicBool>,
    origin: QueryOrigin,
) -> Result<RawOutput, String> {
    let mut guard = session.query_conn.lock().await;
    let conn = guard
        .as_mut()
        .ok_or_else(|| "This connection is closed.".to_string())?;
    let started = Instant::now();
    let outcome = conn.run(sql, limit, cancel).await;
    record(session, sql, origin, started, &outcome);
    outcome
}

async fn load_namespaces(session: &Session) -> Result<NamespaceList, String> {
    let dialect = dialect(session.driver);
    let items: Vec<String> = pool_run(session, dialect.namespaces_sql(), usize::MAX, QueryOrigin::Schema)
        .await?
        .text_rows()
        .into_iter()
        .filter_map(|row| row.into_iter().next().flatten())
        .collect();
    Ok(NamespaceList {
        items,
        current: session.namespace(),
    })
}

fn pick_namespace(items: &[String], reported: &str, system: &[&str], preferred: &str) -> String {
    if !reported.is_empty() && items.iter().any(|item| item == reported) {
        return reported.to_string();
    }
    if !preferred.is_empty() && items.iter().any(|item| item == preferred) {
        return preferred.to_string();
    }
    items
        .iter()
        .find(|item| !system.contains(&item.as_str()))
        .or_else(|| items.first())
        .cloned()
        .unwrap_or_default()
}

#[tauri::command]
pub async fn test_connection(
    connection: ConnectionEntry,
    password: Option<String>,
    ssh_secret: Option<String>,
) -> Result<String, String> {
    let entry = sanitize_connection(connection)?;
    let password = resolve_password(&entry, password)?;
    let (target, tunnel) = open_tunnel(&entry, ssh_secret).await?;
    let outcome = match target.driver {
        Driver::Mysql => db::mysql::test(&target, password.as_deref()).await,
        Driver::Postgres => db::postgres::test(&target, password.as_deref()).await,
        Driver::Sqlite => db::sqlite::test(&target).await,
    };
    let outcome = outcome.map_err(|err| explain(tunnel.as_ref(), err));
    if let Some(tunnel) = tunnel {
        tunnel.close().await;
    }
    outcome
}

#[tauri::command]
pub async fn create_sqlite_database(path: String) -> Result<(), String> {
    if path.trim().is_empty() {
        return Err("Choose where to create the database file.".into());
    }
    db::sqlite::create(path.trim()).await
}

#[tauri::command]
pub async fn connect(
    state: State<'_, AppState>,
    sessions: State<'_, SessionStore>,
    results: State<'_, ResultStore>,
    connection_id: String,
    password: Option<String>,
) -> Result<SessionInfo, String> {
    let entry = {
        let data = state.data.lock().map_err(|err| err.to_string())?;
        data.find_connection(&connection_id)
            .cloned()
            .ok_or_else(|| "Connection not found".to_string())?
    };
    let password = resolve_password(&entry, password)?;
    if let Some(previous) = sessions.remove(&connection_id).await {
        previous.close().await;
        results.remove_connection(&connection_id);
    }
    let (target, tunnel) = open_tunnel(&entry, None).await?;
    let opened = match target.driver {
        Driver::Mysql => db::mysql::open(&target, password.as_deref()).await,
        Driver::Postgres => db::postgres::open(&target, password.as_deref()).await,
        Driver::Sqlite => db::sqlite::open(&target).await,
    };
    let opened = match opened {
        Ok(opened) => opened,
        Err(err) => {
            let err = explain(tunnel.as_ref(), err);
            if let Some(tunnel) = tunnel {
                tunnel.close().await;
            }
            return Err(err);
        }
    };
    let session = Session {
        name: entry.name.clone(),
        driver: entry.driver,
        pool: opened.pool,
        query_conn: tokio::sync::Mutex::new(Some(opened.conn)),
        backend_id: opened.backend_id,
        cancel: AtomicBool::new(false),
        namespace: Mutex::new(String::new()),
        tunnel,
    };
    let dialect = dialect(entry.driver);
    let setup = async {
        let version = first_text(&pool_run(&session, dialect.version_sql(), 1, QueryOrigin::Schema).await?);
        let reported = first_text(
            &conn_run(&session, dialect.current_namespace_sql(), 1, None, QueryOrigin::Schema).await?,
        );
        let mut namespaces = load_namespaces(&session).await?;
        let current = pick_namespace(
            &namespaces.items,
            &reported,
            dialect.system_namespaces(),
            &entry.database,
        );
        session.set_namespace(&current);
        if current != reported && !current.is_empty() {
            if let Some(sql) = dialect.use_namespace_sql(&current) {
                conn_run(&session, &sql, 0, None, QueryOrigin::Schema).await?;
            }
        }
        namespaces.current = current;
        Ok::<_, String>(SessionInfo {
            server_version: version,
            namespaces,
            namespace_label: dialect.namespace_label().into(),
        })
    };
    match setup.await {
        Ok(info) => {
            if let Some(stale) = sessions.insert(&connection_id, session).await {
                stale.close().await;
            }
            Ok(info)
        }
        Err(err) => {
            session.close().await;
            Err(err)
        }
    }
}

#[tauri::command]
pub async fn disconnect(
    sessions: State<'_, SessionStore>,
    results: State<'_, ResultStore>,
    connection_id: String,
) -> Result<(), String> {
    results.remove_connection(&connection_id);
    if let Some(session) = sessions.remove(&connection_id).await {
        session.close().await;
    }
    Ok(())
}

#[tauri::command]
pub async fn list_databases(
    sessions: State<'_, SessionStore>,
    connection_id: String,
) -> Result<NamespaceList, String> {
    let session = sessions.get(&connection_id).await?;
    load_namespaces(&session).await
}

#[tauri::command]
pub async fn set_database(
    sessions: State<'_, SessionStore>,
    connection_id: String,
    namespace: String,
) -> Result<(), String> {
    let session = sessions.get(&connection_id).await?;
    if let Some(sql) = dialect(session.driver).use_namespace_sql(&namespace) {
        conn_run(&session, &sql, 0, None, QueryOrigin::Schema).await?;
    }
    session.set_namespace(&namespace);
    Ok(())
}

#[tauri::command]
pub async fn list_tables(
    sessions: State<'_, SessionStore>,
    connection_id: String,
    namespace: String,
) -> Result<Vec<TableInfo>, String> {
    let session = sessions.get(&connection_id).await?;
    let sql = dialect(session.driver).tables_sql(&namespace);
    let output = pool_run(&session, &sql, usize::MAX, QueryOrigin::Schema).await?;
    Ok(output
        .text_rows()
        .into_iter()
        .map(|row| TableInfo {
            name: text_at(&row, 0),
            kind: match text_at(&row, 1).to_ascii_lowercase().as_str() {
                "view" => "view".into(),
                _ => "table".into(),
            },
        })
        .filter(|table| !table.name.is_empty())
        .collect())
}

#[tauri::command]
pub async fn table_structure(
    sessions: State<'_, SessionStore>,
    connection_id: String,
    namespace: String,
    table: String,
) -> Result<TableStructure, String> {
    let session = sessions.get(&connection_id).await?;
    let dialect = dialect(session.driver);
    let columns_sql = dialect.columns_sql(&namespace, &table);
    let columns = pool_run(&session, &columns_sql, usize::MAX, QueryOrigin::Schema)
        .await?
        .rows
        .into_iter()
        .map(|row| {
            let text = db::texts(&row);
            ColumnDetail {
                name: text_at(&text, 0),
                data_type: text_at(&text, 1),
                nullable: text_at(&text, 2).eq_ignore_ascii_case("YES"),
                default_value: text.get(3).cloned().flatten(),
                primary_key: row.get(4).is_some_and(CellValue::is_truthy),
                extra: text_at(&text, 5),
            }
        })
        .collect();
    let indexes_sql = dialect.indexes_sql(&namespace, &table);
    let indexes = pool_run(&session, &indexes_sql, usize::MAX, QueryOrigin::Schema)
        .await?
        .rows
        .into_iter()
        .map(|row| {
            let text = db::texts(&row);
            IndexInfo {
                name: text_at(&text, 0),
                columns: dialect.index_columns(text_at(&text, 1)),
                unique: row.get(2).is_some_and(CellValue::is_truthy),
                primary: row.get(3).is_some_and(CellValue::is_truthy),
            }
        })
        .collect();
    Ok(TableStructure { columns, indexes })
}

#[tauri::command]
pub async fn schema_columns(
    sessions: State<'_, SessionStore>,
    connection_id: String,
    namespace: String,
) -> Result<Vec<SchemaColumn>, String> {
    let session = sessions.get(&connection_id).await?;
    let sql = dialect(session.driver).schema_columns_sql(&namespace);
    let output = pool_run(&session, &sql, usize::MAX, QueryOrigin::Schema).await?;
    Ok(output
        .text_rows()
        .into_iter()
        .map(|row| SchemaColumn {
            table: text_at(&row, 0),
            column: text_at(&row, 1),
        })
        .collect())
}

#[tauri::command]
pub async fn browse_table(
    sessions: State<'_, SessionStore>,
    connection_id: String,
    request: BrowseRequest,
) -> Result<BrowseResult, String> {
    let session = sessions.get(&connection_id).await?;
    let dialect = dialect(session.driver);
    let table = dialect.qualified(&request.namespace, &request.table);
    let order = match request.order_by.as_deref().filter(|column| !column.is_empty()) {
        Some(column) => format!(
            " ORDER BY {} {}",
            dialect.quote_ident(column),
            request.order_dir.unwrap_or(db::SortDirection::Asc).sql()
        ),
        None => String::new(),
    };
    let limit = request.limit.clamp(1, BROWSE_LIMIT_MAX);
    let sql = format!(
        "SELECT * FROM {table}{order} LIMIT {limit} OFFSET {}",
        request.offset
    );
    let started = Instant::now();
    let output = pool_run(&session, &sql, limit as usize, QueryOrigin::Browse).await?;
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let total = if request.count {
        pool_run(&session, &format!("SELECT COUNT(*) FROM {table}"), 1, QueryOrigin::Browse)
            .await
            .ok()
            .and_then(|count| count.rows.first()?.first()?.as_i64())
            .and_then(|count| u64::try_from(count).ok())
    } else {
        None
    };
    Ok(BrowseResult {
        columns: output.columns,
        rows: output.rows,
        offset: request.offset,
        total,
        duration_ms,
    })
}

#[tauri::command]
pub async fn run_query(
    state: State<'_, AppState>,
    sessions: State<'_, SessionStore>,
    results: State<'_, ResultStore>,
    connection_id: String,
    statements: Vec<String>,
) -> Result<Vec<StatementResult>, String> {
    let session = sessions.get(&connection_id).await?;
    let limit = state
        .data
        .lock()
        .map(|data| data.query_row_limit as usize)
        .map_err(|err| err.to_string())?;
    session.cancel.store(false, Ordering::Relaxed);
    let mut output = Vec::new();
    for sql in statements.iter().map(|sql| sql.trim()).filter(|sql| !sql.is_empty()) {
        if session.cancelled() {
            break;
        }
        let started = Instant::now();
        let outcome = conn_run(&session, sql, limit, Some(&session.cancel), QueryOrigin::Editor).await;
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        match outcome {
            Ok(raw) if raw.columns.is_empty() => output.push(StatementResult {
                sql: sql.to_string(),
                result_id: None,
                columns: Vec::new(),
                rows: Vec::new(),
                row_count: 0,
                truncated: false,
                rows_affected: Some(raw.rows_affected),
                duration_ms,
                error: None,
            }),
            Ok(raw) => {
                let row_count = raw.rows.len();
                let first_page = raw.rows[..row_count.min(FIRST_PAGE)].to_vec();
                let result_id = (row_count > FIRST_PAGE).then(|| results.insert(&connection_id, raw.rows));
                output.push(StatementResult {
                    sql: sql.to_string(),
                    result_id,
                    columns: raw.columns,
                    rows: first_page,
                    row_count,
                    truncated: raw.truncated,
                    rows_affected: None,
                    duration_ms,
                    error: None,
                });
            }
            Err(err) => {
                let error = if session.cancelled() {
                    "Query cancelled.".to_string()
                } else {
                    err
                };
                output.push(StatementResult {
                    sql: sql.to_string(),
                    result_id: None,
                    columns: Vec::new(),
                    rows: Vec::new(),
                    row_count: 0,
                    truncated: false,
                    rows_affected: None,
                    duration_ms,
                    error: Some(error),
                });
                break;
            }
        }
    }
    Ok(output)
}

#[tauri::command]
pub fn fetch_rows(
    results: State<'_, ResultStore>,
    result_id: String,
    offset: usize,
    limit: usize,
) -> Result<Vec<RowValues>, String> {
    results.window(&result_id, offset, limit.min(5_000))
}

#[tauri::command]
pub fn close_results(results: State<'_, ResultStore>, result_ids: Vec<String>) {
    results.remove(&result_ids);
}

#[tauri::command]
pub async fn cancel_query(sessions: State<'_, SessionStore>, connection_id: String) -> Result<(), String> {
    let session = sessions.get(&connection_id).await?;
    session.cancel.store(true, Ordering::Relaxed);
    match (&session.pool, session.backend_id) {
        (Pool::MySql(_), Some(id)) => {
            pool_run(&session, &format!("KILL QUERY {id}"), 0, QueryOrigin::Schema).await?;
        }
        (Pool::Postgres(_), Some(id)) => {
            pool_run(&session, &format!("SELECT pg_cancel_backend({id})"), 1, QueryOrigin::Schema).await?;
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_a_sensible_namespace() {
        let items: Vec<String> = ["information_schema", "app", "mysql", "shop"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let system = ["information_schema", "mysql"];
        assert_eq!(pick_namespace(&items, "shop", &system, ""), "shop");
        assert_eq!(pick_namespace(&items, "", &system, "shop"), "shop");
        assert_eq!(pick_namespace(&items, "", &system, ""), "app");
        assert_eq!(pick_namespace(&[], "", &system, ""), "");
    }
}
