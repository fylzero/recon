use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::commands::{sanitize_connection, AppState};
use crate::db::ssh::Tunnel;
use crate::db::{
    self, dialect, first_text, text_at, BrowseRequest, BrowseResult, CellValue, ColumnDetail,
    ColumnMeta, IndexInfo, NamespaceList, Pool, RawOutput, ResultStore, RowValues, SaveRequest,
    SchemaColumn, Session, SessionStore, TableInfo, TableStructure,
};
use crate::models::{ConnectionEntry, Driver};
use crate::query_log::{self, QueryOrigin, QueryRecord};
use crate::secrets;

const FIRST_PAGE: usize = 200;
const BROWSE_LIMIT_MAX: u64 = 5_000;
const CONNECTION_LOST_EVENT: &str = "connection-lost";
const CONNECTION_RESTORED_EVENT: &str = "connection-restored";
const EDITOR_RECONNECTED: &str = "The connection to the database server was lost while running this statement, \
                                  so it may not have run. Recon reconnected, but any open transaction or session \
                                  settings were reset.";

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionEvent<'a> {
    connection_id: &'a str,
    error: Option<&'a str>,
}

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
    let (session, info) = open_session(entry, password, "").await?;
    if let Some(stale) = sessions.insert(&connection_id, session).await {
        stale.close().await;
    }
    Ok(info)
}

/**
 * Opens the tunnel, pool, and editor connection for `entry`. A non-empty
 * `restore` namespace is selected in place of the server's default so a
 * reconnect lands back where the user was.
 */
async fn open_session(
    entry: ConnectionEntry,
    password: Option<String>,
    restore: &str,
) -> Result<(Session, SessionInfo), String> {
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
        entry,
        password,
        pool: opened.pool,
        query_conn: tokio::sync::Mutex::new(Some(opened.conn)),
        backend_id: opened.backend_id,
        cancel: AtomicBool::new(false),
        lost: AtomicBool::new(false),
        namespace: Mutex::new(String::new()),
        tunnel,
    };
    let dialect = dialect(session.driver);
    let setup = async {
        let version = first_text(&pool_run(&session, dialect.version_sql(), 1, QueryOrigin::Schema).await?);
        let reported = first_text(
            &conn_run(&session, dialect.current_namespace_sql(), 1, None, QueryOrigin::Schema).await?,
        );
        let mut namespaces = load_namespaces(&session).await?;
        let wanted = if restore.is_empty() { reported.as_str() } else { restore };
        let current = pick_namespace(
            &namespaces.items,
            wanted,
            dialect.system_namespaces(),
            &session.entry.database,
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
        Ok(info) => Ok((session, info)),
        Err(err) => {
            session.close().await;
            Err(err)
        }
    }
}

/**
 * Re-reads the saved connection so edits made since connecting apply, and
 * falls back to the password typed for the stale session when none is saved.
 */
fn reopen_credentials(
    app: &AppHandle,
    connection_id: &str,
    stale: &Session,
) -> Result<(ConnectionEntry, Option<String>), String> {
    let entry = {
        let state = app.state::<AppState>();
        let data = state.data.lock().map_err(|err| err.to_string())?;
        data.find_connection(connection_id)
            .cloned()
            .unwrap_or_else(|| stale.entry.clone())
    };
    let saved = resolve_password(&entry, None)?;
    Ok((entry, saved.or_else(|| stale.password.clone())))
}

async fn rebuild(
    app: &AppHandle,
    connection_id: &str,
    stale: &Arc<Session>,
    password: Option<String>,
) -> Result<(Arc<Session>, SessionInfo), String> {
    let (entry, saved) = reopen_credentials(app, connection_id, stale)?;
    let password = password.filter(|value| !value.is_empty()).or(saved);
    let (session, info) = open_session(entry, password, &stale.namespace()).await?;
    match app.state::<SessionStore>().replace(connection_id, stale, session).await {
        Ok(next) => {
            let stale = stale.clone();
            tauri::async_runtime::spawn(async move { stale.close().await });
            Ok((next, info))
        }
        Err(unused) => {
            unused.close().await;
            Err("This connection was closed.".into())
        }
    }
}

/**
 * Called after an operation fails because the connection dropped. Returns a
 * working session, or marks the connection lost and tells the frontend.
 */
async fn recover(app: &AppHandle, connection_id: &str, stale: &Arc<Session>) -> Option<Arc<Session>> {
    let sessions = app.state::<SessionStore>();
    let _guard = sessions.reconnect_guard().await;
    let current = sessions.current(connection_id).await?;
    if !Arc::ptr_eq(&current, stale) || current.is_lost() {
        return (!current.is_lost()).then_some(current);
    }
    match rebuild(app, connection_id, stale, None).await {
        Ok((session, _)) => {
            let event = ConnectionEvent { connection_id, error: None };
            let _ = app.emit(CONNECTION_RESTORED_EVENT, event);
            Some(session)
        }
        Err(err) => {
            stale.mark_lost();
            let event = ConnectionEvent { connection_id, error: Some(&err) };
            let _ = app.emit(CONNECTION_LOST_EVENT, event);
            None
        }
    }
}

/**
 * Runs `op` against the open session. If the connection turns out to be
 * gone, reconnects once and runs `op` again, so only operations that are
 * safe to repeat should go through here.
 */
async fn with_session<T, F, Fut>(app: &AppHandle, connection_id: &str, op: F) -> Result<T, String>
where
    F: Fn(Arc<Session>) -> Fut,
    Fut: Future<Output = Result<T, String>>,
{
    let session = app.state::<SessionStore>().get(connection_id).await?;
    match op(session.clone()).await {
        Err(err) if db::is_connection_lost(&err) => match recover(app, connection_id, &session).await {
            Some(next) => op(next).await,
            None => Err(err),
        },
        outcome => outcome,
    }
}

#[tauri::command]
pub async fn reconnect(
    app: AppHandle,
    connection_id: String,
    password: Option<String>,
) -> Result<SessionInfo, String> {
    let sessions = app.state::<SessionStore>();
    let _guard = sessions.reconnect_guard().await;
    let stale = sessions
        .current(&connection_id)
        .await
        .ok_or_else(|| "This connection is not open.".to_string())?;
    let (_, info) = rebuild(&app, &connection_id, &stale, password).await?;
    Ok(info)
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
pub async fn list_databases(app: AppHandle, connection_id: String) -> Result<NamespaceList, String> {
    with_session(&app, &connection_id, |session| async move { load_namespaces(&session).await }).await
}

#[tauri::command]
pub async fn set_database(app: AppHandle, connection_id: String, namespace: String) -> Result<(), String> {
    let namespace = namespace.as_str();
    with_session(&app, &connection_id, move |session| use_namespace(session, namespace)).await
}

async fn use_namespace(session: Arc<Session>, namespace: &str) -> Result<(), String> {
    if let Some(sql) = dialect(session.driver).use_namespace_sql(namespace) {
        conn_run(&session, &sql, 0, None, QueryOrigin::Schema).await?;
    }
    session.set_namespace(namespace);
    Ok(())
}

#[tauri::command]
pub async fn list_tables(app: AppHandle, connection_id: String, namespace: String) -> Result<Vec<TableInfo>, String> {
    let namespace = namespace.as_str();
    with_session(&app, &connection_id, move |session| tables_in(session, namespace)).await
}

async fn tables_in(session: Arc<Session>, namespace: &str) -> Result<Vec<TableInfo>, String> {
    let sql = dialect(session.driver).tables_sql(namespace);
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
    app: AppHandle,
    connection_id: String,
    namespace: String,
    table: String,
) -> Result<TableStructure, String> {
    let (namespace, table) = (namespace.as_str(), table.as_str());
    with_session(&app, &connection_id, move |session| structure_of(session, namespace, table)).await
}

async fn structure_of(session: Arc<Session>, namespace: &str, table: &str) -> Result<TableStructure, String> {
    let dialect = dialect(session.driver);
    let columns_sql = dialect.columns_sql(namespace, table);
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
    let indexes_sql = dialect.indexes_sql(namespace, table);
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
    app: AppHandle,
    connection_id: String,
    namespace: String,
) -> Result<Vec<SchemaColumn>, String> {
    let namespace = namespace.as_str();
    with_session(&app, &connection_id, move |session| columns_in(session, namespace)).await
}

async fn columns_in(session: Arc<Session>, namespace: &str) -> Result<Vec<SchemaColumn>, String> {
    let sql = dialect(session.driver).schema_columns_sql(namespace);
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
    app: AppHandle,
    connection_id: String,
    request: BrowseRequest,
) -> Result<BrowseResult, String> {
    let request = &request;
    with_session(&app, &connection_id, move |session| browse(session, request)).await
}

async fn browse(session: Arc<Session>, request: &BrowseRequest) -> Result<BrowseResult, String> {
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
pub async fn save_table_changes(
    app: AppHandle,
    connection_id: String,
    requests: Vec<SaveRequest>,
) -> Result<usize, String> {
    let requests = requests.as_slice();
    let repeatable = requests.iter().all(|request| {
        request.inserts.is_empty()
            && request.columns.is_empty()
            && request.new_columns.is_empty()
            && request.indexes.is_empty()
            && request.new_indexes.is_empty()
    });
    if repeatable {
        return with_session(&app, &connection_id, move |session| save(session, requests)).await;
    }
    let session = app.state::<SessionStore>().get(&connection_id).await?;
    match save(session.clone(), requests).await {
        Err(err) if db::is_connection_lost(&err) => {
            let _ = recover(&app, &connection_id, &session).await;
            Err(format!(
                "{err} Recon reconnected but didn't retry, because new rows or structure changes may already be saved. Reload to check before saving again."
            ))
        }
        outcome => outcome,
    }
}

/**
 * Row edits are primary-key updates to absolute values inside one
 * transaction, so repeating them after a dropped connection cannot apply
 * anything twice. Structure changes run after every row update and insert
 * because those still use the old column names, and new indexes run last so
 * they can use new columns.
 */
async fn save(session: Arc<Session>, requests: &[SaveRequest]) -> Result<usize, String> {
    let dialect = dialect(session.driver);
    let mut statements = Vec::new();
    let mut schema_statements = Vec::new();
    for request in requests {
        let table = dialect.qualified(&request.namespace, &request.table);
        for update in &request.updates {
            statements.push(db::update_statement(session.driver, &table, update)?);
        }
        for insert in &request.inserts {
            statements.push(db::insert_statement(session.driver, &table, insert));
        }
        if !request.columns.is_empty() {
            let current = if session.driver == Driver::Mysql {
                let sql = db::mysql::column_definitions_sql(&request.namespace, &request.table);
                let output = pool_run(&session, &sql, usize::MAX, QueryOrigin::Schema).await?;
                db::mysql::column_definitions(&output)
            } else {
                Vec::new()
            };
            for change in &request.columns {
                let existing = current.iter().find(|column| column.name == change.column);
                schema_statements.extend(db::alter_statements(session.driver, &table, change, existing)?);
            }
        }
        for column in &request.new_columns {
            schema_statements.push(db::add_column_statement(session.driver, &table, column)?);
        }
        if !request.indexes.is_empty() {
            let sql = dialect.index_definitions_sql(&request.namespace, &request.table);
            let output = pool_run(&session, &sql, usize::MAX, QueryOrigin::Schema).await?;
            let current = db::index_definitions(session.driver, &output);
            for change in &request.indexes {
                let existing = current.iter().find(|index| index.name == change.index);
                schema_statements.extend(db::index_statements(
                    session.driver,
                    &request.namespace,
                    &table,
                    change,
                    existing,
                )?);
            }
        }
        for index in &request.new_indexes {
            schema_statements.push(db::create_index_statement(
                session.driver,
                &request.namespace,
                &table,
                &request.table,
                index,
            )?);
        }
    }
    let changed = requests
        .iter()
        .map(|request| {
            request.updates.len()
                + request.inserts.len()
                + request.columns.len()
                + request.new_columns.len()
                + request.indexes.len()
                + request.new_indexes.len()
        })
        .sum();
    let alters_schema = !schema_statements.is_empty();
    statements.extend(schema_statements);
    if statements.is_empty() {
        return Ok(0);
    }
    let started = Instant::now();
    let outcome = session.pool.apply(&statements).await.map_err(|err| {
        if alters_schema && session.driver == Driver::Mysql && !db::is_connection_lost(&err) {
            format!(
                "{err} MySQL commits each structure change as it runs, so changes before this one may already be saved. Reload to check."
            )
        } else {
            err
        }
    });
    let sql = statements
        .iter()
        .map(|statement| statement.display.as_str())
        .collect::<Vec<_>>()
        .join(";\n");
    let namespace = session.namespace();
    query_log::record(QueryRecord {
        connection: &session.name,
        driver: session.driver.label(),
        database: &namespace,
        sql: &sql,
        origin: QueryOrigin::Edit,
        duration: started.elapsed(),
        outcome: match &outcome {
            Ok(()) => Ok(Some(statements.len() as u64)),
            Err(err) => Err(err.as_str()),
        },
    });
    outcome.map(|()| changed)
}

#[tauri::command]
pub async fn run_query(
    app: AppHandle,
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
                } else if db::is_connection_lost(&err) && recover(&app, &connection_id, &session).await.is_some() {
                    EDITOR_RECONNECTED.to_string()
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
