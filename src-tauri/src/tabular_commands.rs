use std::collections::HashSet;
use std::fs::{self, File};
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use futures_util::stream::BoxStream;
use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use sqlx::{Column, Executor, Row};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::mpsc;

use crate::db::dump::{self, DumpSummary, DumpTable};
use crate::db::tabular::{
    self, Format, ImportValue, Position, SourceSpec, TabularOptions, TabularWriter, TargetColumn, TypeGuess,
};
use crate::db::{
    describe_error, dialect, mysql, postgres, sqlite, BrowseRequest, CellValue, Conn, EditValue, ResultStore, Session,
    SessionStore,
};
use crate::db_commands::{export_statement, table_columns, tables_in};
use crate::models::Driver;
use crate::query_log::{self, QueryOrigin, QueryRecord};
use crate::transfer_commands::{
    file_name, open_sql_file, sibling, ExportProgressEvent, ExportResult, Sink, SqlFile, Throttle, Transfer,
    TransferStore, EXPORT_PROGRESS_EVENT, IMPORT_CANCELLED, IMPORT_PROGRESS_EVENT,
};

const REPORT_EVERY: u64 = 500;
const PREVIEW_ROWS: usize = 20;
const GUESS_ROWS: usize = 1_000;
const BATCH_ROWS: usize = 500;
const BATCH_BYTES: usize = 512 * 1024;
const SAVEPOINT: &str = "recon_import";

fn yes() -> bool {
    true
}

fn file_safe(name: &str) -> String {
    name.chars()
        .map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '-' } else { c })
        .collect()
}

fn column_names<C: Column>(columns: &[C]) -> Vec<String> {
    columns.iter().map(|column| column.name().to_string()).collect()
}

async fn drain<R: Row>(
    mut stream: BoxStream<'_, Result<R, sqlx::Error>>,
    cell: fn(&R, usize) -> CellValue,
    writer: &mut TabularWriter<'_>,
    cancel: &AtomicBool,
    report: &mut (dyn FnMut(u64) + Send),
) -> Result<(), String> {
    let mut values = Vec::new();
    while let Some(row) = stream.try_next().await.map_err(describe_error)? {
        if cancel.load(Ordering::Relaxed) {
            return Err(dump::CANCELLED.into());
        }
        if !writer.has_columns() {
            writer.set_columns(column_names(row.columns()))?;
        }
        values.clear();
        values.extend((0..row.len()).map(|index| cell(&row, index)));
        writer.write_row(&values)?;
        if writer.rows() % REPORT_EVERY == 0 {
            report(writer.rows());
        }
    }
    report(writer.rows());
    Ok(())
}

/**
 * Streams the rows of `sql` into `writer`, so no result has to fit in memory.
 * SQLite binds `params`; the other drivers get SQL with its values inlined.
 * An empty result still gets its column names from the statement.
 */
pub(crate) async fn write_query(
    conn: &mut Conn,
    sql: &str,
    params: &[EditValue],
    writer: &mut TabularWriter<'_>,
    cancel: &AtomicBool,
    report: &mut (dyn FnMut(u64) + Send),
) -> Result<(), String> {
    match conn {
        Conn::MySql(conn) => {
            drain(sqlx::raw_sql(sql).fetch(&mut *conn), mysql::cell, writer, cancel, report).await?;
            if !writer.has_columns() {
                if let Ok(described) = (&mut *conn).describe(sql).await {
                    writer.set_columns(column_names(described.columns()))?;
                }
            }
        }
        Conn::Postgres(conn) => {
            drain(sqlx::raw_sql(sql).fetch(&mut *conn), postgres::cell, writer, cancel, report).await?;
            if !writer.has_columns() {
                if let Ok(described) = (&mut *conn).describe(sql).await {
                    writer.set_columns(column_names(described.columns()))?;
                }
            }
        }
        Conn::Sqlite(conn) => {
            let mut query = sqlx::query::<sqlx::Sqlite>(sql);
            for param in params {
                query = match param {
                    EditValue::Null | EditValue::Now(_) => query.bind(None::<String>),
                    EditValue::Bool(value) => query.bind(*value),
                    EditValue::Int(value) => query.bind(*value),
                    EditValue::Float(value) => query.bind(*value),
                    EditValue::Text(value) => query.bind(value.as_str()),
                };
            }
            drain(query.fetch(&mut *conn), sqlite::cell, writer, cancel, report).await?;
            if !writer.has_columns() {
                if let Ok(described) = (&mut *conn).describe(sql).await {
                    writer.set_columns(column_names(described.columns()))?;
                }
            }
        }
    }
    Ok(())
}

/// Statements that make every table in one export read from the same moment.
fn snapshot_sql(driver: Driver) -> &'static [&'static str] {
    match driver {
        Driver::Postgres => &["BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY"],
        Driver::Mysql => &[
            "SET SESSION TRANSACTION ISOLATION LEVEL REPEATABLE READ",
            "START TRANSACTION WITH CONSISTENT SNAPSHOT",
        ],
        Driver::Sqlite => &["BEGIN"],
    }
}

/// One file per table, with names made unique ignoring case since macOS folders do.
fn table_files(folder: &Path, tables: &[DumpTable], options: &TabularOptions) -> Vec<PathBuf> {
    let mut used = HashSet::new();
    tables
        .iter()
        .map(|table| {
            let base = file_safe(&table.name);
            let mut name = format!("{base}.{}", options.extension());
            let mut next = 2;
            while !used.insert(name.to_lowercase()) {
                name = format!("{base}-{next}.{}", options.extension());
                next += 1;
            }
            folder.join(name)
        })
        .collect()
}

/**
 * Writes each table's rows as CSV or JSON. One table goes to `path`; several
 * go into the folder at `path`, one file each. Every file is written next to
 * its destination and only renamed into place once all of them succeed.
 */
pub(crate) async fn export_tables(
    app: &AppHandle,
    session: &Session,
    transfer: &Transfer<'_>,
    namespace: &str,
    tables: &[DumpTable],
    options: &TabularOptions,
    path: &Path,
) -> Result<ExportResult, String> {
    let targets = if tables.len() == 1 {
        vec![path.to_path_buf()]
    } else {
        if !path.is_dir() {
            return Err(format!("{} isn't a folder. Choose a folder to save one file per table in.", file_name(path)));
        }
        table_files(path, tables, options)
    };
    let parts: Vec<PathBuf> = targets.iter().map(|target| sibling(target, "part")).collect();
    let dialect = dialect(session.driver);
    let mut summary = DumpSummary::default();

    let outcome = async {
        let mut conn = session.pool.detached().await?;
        let result = async {
            for sql in snapshot_sql(session.driver) {
                conn.execute(sql).await?;
            }
            let mut throttle = Throttle(None);
            for (index, table) in tables.iter().enumerate() {
                let file = File::create(&parts[index])
                    .map_err(|err| format!("Could not create {}: {err}", file_name(&targets[index])))?;
                let mut sink = Sink::new(file, options.gzip);
                let mut report = |rows: u64| {
                    if throttle.ready(rows == 0) {
                        let _ = app.emit(
                            EXPORT_PROGRESS_EVENT,
                            ExportProgressEvent {
                                transfer_id: transfer.transfer_id,
                                table: &table.name,
                                index: index + 1,
                                total: tables.len(),
                                rows,
                            },
                        );
                    }
                };
                report(0);
                let mut writer = TabularWriter::new(&mut sink, options);
                let sql = format!("SELECT * FROM {}", dialect.qualified(namespace, &table.name));
                write_query(&mut conn, &sql, &[], &mut writer, &transfer.cancel, &mut report).await?;
                summary.rows += writer.finish()?;
                sink.finish().map_err(|err| format!("Could not write the export file: {err}"))?;
                summary.tables += 1;
            }
            conn.execute("COMMIT").await?;
            Ok::<_, String>(())
        }
        .await;
        conn.close().await;
        result
    }
    .await;

    let finished = outcome.and_then(|()| {
        for (part, target) in parts.iter().zip(&targets) {
            fs::rename(part, target).map_err(|err| format!("Could not save {}: {err}", file_name(target)))?;
        }
        Ok(())
    });
    if let Err(err) = finished {
        for part in &parts {
            let _ = fs::remove_file(part);
        }
        return Err(err);
    }
    let bytes = targets.iter().map(|target| fs::metadata(target).map(|meta| meta.len()).unwrap_or(0)).sum();
    Ok(ExportResult { summary, bytes, path: path.display().to_string() })
}

fn single_result(rows: u64, path: String) -> ExportResult {
    let bytes = fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
    ExportResult { summary: DumpSummary { tables: 1, rows, skipped: Vec::new() }, bytes, path }
}

/** A table view's rows, with its filters and sort, as CSV or JSON. */
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn export_browse(
    app: AppHandle,
    sessions: State<'_, SessionStore>,
    transfers: State<'_, TransferStore>,
    connection_id: String,
    transfer_id: String,
    request: BrowseRequest,
    page_only: bool,
    options: TabularOptions,
    path: String,
) -> Result<ExportResult, String> {
    let session = sessions.get(&connection_id).await?;
    let statement = export_statement(&session, &request, page_only).await?;
    let (sql, params) = match session.driver {
        Driver::Sqlite => statement.bound(),
        driver => (statement.inline(driver), Vec::new()),
    };
    let transfer = transfers.start(&transfer_id);
    let target = PathBuf::from(&path);
    let part = sibling(&target, "part");

    let outcome = async {
        let file = File::create(&part).map_err(|err| format!("Could not create {}: {err}", file_name(&target)))?;
        let mut sink = Sink::new(file, options.gzip);
        let mut conn = session.pool.detached().await?;
        let mut throttle = Throttle(None);
        let mut report = |rows: u64| {
            if throttle.ready(false) {
                let _ = app.emit(
                    EXPORT_PROGRESS_EVENT,
                    ExportProgressEvent { transfer_id: &transfer_id, table: &request.table, index: 1, total: 1, rows },
                );
            }
        };
        let written = async {
            let mut writer = TabularWriter::new(&mut sink, &options);
            write_query(&mut conn, &sql, &params, &mut writer, &transfer.cancel, &mut report).await?;
            writer.finish()
        }
        .await;
        conn.close().await;
        let rows = written?;
        sink.finish().map_err(|err| format!("Could not write the export file: {err}"))?;
        fs::rename(&part, &target).map_err(|err| format!("Could not save {}: {err}", file_name(&target)))?;
        Ok::<_, String>(rows)
    }
    .await;
    match outcome {
        Ok(rows) => Ok(single_result(rows, path)),
        Err(err) => {
            let _ = fs::remove_file(&part);
            Err(err)
        }
    }
}

fn write_held_rows(columns: Vec<String>, rows: &[Vec<CellValue>], options: &TabularOptions, path: &Path) -> Result<u64, String> {
    let part = sibling(path, "part");
    let written = (|| {
        let file = File::create(&part).map_err(|err| format!("Could not create {}: {err}", file_name(path)))?;
        let mut sink = Sink::new(file, options.gzip);
        let mut writer = TabularWriter::new(&mut sink, options);
        writer.set_columns(columns)?;
        for row in rows {
            writer.write_row(row)?;
        }
        let count = writer.finish()?;
        sink.finish().map_err(|err| format!("Could not write the export file: {err}"))?;
        fs::rename(&part, path).map_err(|err| format!("Could not save {}: {err}", file_name(path)))?;
        Ok(count)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&part);
    }
    written
}

/** The rows a query result holds, which stop at the query row limit, as CSV or JSON. */
#[tauri::command]
pub async fn export_result(
    results: State<'_, ResultStore>,
    result_id: String,
    columns: Vec<String>,
    options: TabularOptions,
    path: String,
) -> Result<ExportResult, String> {
    let rows = results.rows(&result_id)?;
    let target = path.clone();
    let count = tauri::async_runtime::spawn_blocking(move || write_held_rows(columns, &rows, &options, Path::new(&target)))
        .await
        .map_err(|err| err.to_string())??;
    Ok(single_result(count, path))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportFileOptions {
    /// Picked from the file name, or from its first character, when absent.
    #[serde(default)]
    pub format: Option<Format>,
    /// Detected from the first line when absent.
    #[serde(default)]
    pub delimiter: Option<char>,
    #[serde(default = "yes")]
    pub header: bool,
    /// Unquoted CSV fields that mean NULL.
    #[serde(default)]
    pub null_markers: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreview {
    pub file_name: String,
    pub format: Format,
    pub delimiter: char,
    pub columns: Vec<String>,
    /// A column type for each column, guessed from the sampled rows, for creating a table.
    pub types: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub sampled: usize,
    pub total_bytes: u64,
}

fn base_name(path: &str) -> String {
    let lower = path.to_ascii_lowercase();
    lower.strip_suffix(".gz").unwrap_or(&lower).to_string()
}

fn resolve_spec(path: &str, file: &mut SqlFile, options: &ImportFileOptions) -> Result<SourceSpec, String> {
    let base = base_name(path);
    let sample = file
        .reader
        .fill_buf()
        .map_err(|err| format!("Could not read {}: {err}", file.name))?;
    let format = options.format.unwrap_or_else(|| {
        if [".json", ".ndjson", ".jsonl"].iter().any(|ext| base.ends_with(ext)) {
            Format::Json
        } else if [".csv", ".tsv", ".txt"].iter().any(|ext| base.ends_with(ext)) {
            Format::Csv
        } else if tabular::looks_like_json(sample) {
            Format::Json
        } else {
            Format::Csv
        }
    });
    let delimiter = match options.delimiter {
        Some(delimiter) if delimiter.is_ascii() && delimiter != '"' && delimiter != '\n' => delimiter as u8,
        Some(_) => return Err("The delimiter has to be a single character other than a quote.".into()),
        None if base.ends_with(".tsv") => b'\t',
        None => tabular::detect_delimiter(sample),
    };
    Ok(SourceSpec { format, delimiter, header: options.header, null_markers: options.null_markers.clone() })
}

pub(crate) fn preview_file(driver: Driver, path: &str, options: &ImportFileOptions) -> Result<ImportPreview, String> {
    let mut file = open_sql_file(path)?;
    let spec = resolve_spec(path, &mut file, options)?;
    let mut columns = Vec::new();
    let mut guesses: Vec<TypeGuess> = Vec::new();
    let mut rows: Vec<Vec<Option<String>>> = Vec::new();
    let mut sampled = 0;
    tabular::read_source(&mut *file.reader, &spec, &mut columns, true, |_, values| {
        if guesses.len() < values.len() {
            guesses.resize(values.len(), TypeGuess::default());
        }
        for (guess, value) in guesses.iter_mut().zip(&values) {
            guess.observe(value);
        }
        if rows.len() < PREVIEW_ROWS {
            rows.push(values.iter().map(ImportValue::preview).collect());
        }
        sampled += 1;
        Ok(sampled < GUESS_ROWS)
    })?;
    guesses.resize(columns.len(), TypeGuess::default());
    for row in &mut rows {
        row.resize(columns.len(), None);
    }
    Ok(ImportPreview {
        file_name: file.name,
        format: spec.format,
        delimiter: spec.delimiter as char,
        types: guesses.iter().map(|guess| guess.sql_type(driver)).collect(),
        columns,
        rows,
        sampled,
        total_bytes: file.total_bytes,
    })
}

/** The columns and first rows of a CSV or JSON file, for choosing where they go. */
#[tauri::command]
pub async fn preview_import(
    sessions: State<'_, SessionStore>,
    connection_id: String,
    path: String,
    options: ImportFileOptions,
) -> Result<ImportPreview, String> {
    let driver = sessions.get(&connection_id).await?.driver;
    tauri::async_runtime::spawn_blocking(move || preview_file(driver, &path, &options))
        .await
        .map_err(|err| err.to_string())?
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnMapping {
    /// Index into `source_columns`.
    pub source: usize,
    pub target: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewTableColumn {
    pub name: String,
    pub data_type: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewTable {
    pub columns: Vec<NewTableColumn>,
    /// Adds an auto-increment `id` primary key ahead of the file's columns.
    #[serde(default)]
    pub id_column: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportRowsRequest {
    pub namespace: String,
    pub table: String,
    pub path: String,
    pub file: ImportFileOptions,
    /// The columns the preview found, which `mapping` refers to by index.
    pub source_columns: Vec<String>,
    pub mapping: Vec<ColumnMapping>,
    /// Creates `table` first when present.
    #[serde(default)]
    pub create: Option<NewTable>,
    #[serde(default)]
    pub empty_first: bool,
    #[serde(default)]
    pub skip_conflicts: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportRowsResult {
    pub rows: u64,
    pub duration_ms: u64,
    pub created: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportRowsProgress<'a> {
    transfer_id: &'a str,
    rows: u64,
    bytes: u64,
    total_bytes: u64,
}

pub(crate) type Batch = Vec<(Position, Vec<ImportValue>)>;

/// The INSERT every batch shares, and how each target column takes its values.
pub(crate) struct InsertPlan {
    pub driver: Driver,
    pub prefix: String,
    pub suffix: String,
    pub targets: Vec<TargetColumn>,
}

impl InsertPlan {
    fn statement<'a>(&self, rows: impl Iterator<Item = &'a Vec<ImportValue>>) -> String {
        let mut sql = self.prefix.clone();
        for (index, values) in rows.enumerate() {
            if index > 0 {
                sql.push_str(",\n");
            }
            sql.push_str(&tabular::row_values(self.driver, values, &self.targets));
        }
        sql.push_str(&self.suffix);
        sql
    }
}

fn span(first: Position, last: Position) -> String {
    match (first, last) {
        (Position::Line(a), Position::Line(b)) => format!("Lines {a}–{b}"),
        (Position::Item(a), Position::Item(b)) => format!("Items {a}–{b}"),
        (first, _) => first.to_string(),
    }
}

/// Runs a failed batch one row at a time to name the row that failed.
async fn pinpoint(conn: &mut Conn, plan: &InsertPlan, batch: &Batch, err: String) -> String {
    for (position, values) in batch {
        if let Err(row_err) = conn.execute(&plan.statement(std::iter::once(values))).await {
            return format!("{position}: {row_err}");
        }
    }
    match (batch.first(), batch.last()) {
        (Some((first, _)), Some((last, _))) => format!("{}: {err}", span(*first, *last)),
        _ => err,
    }
}

/**
 * Inserts each batch from `batches` under a savepoint inside the caller's
 * transaction, so a failed batch can be retried row by row to find the bad
 * row. Returns how many rows went in.
 */
pub(crate) async fn insert_batches(
    conn: &mut Conn,
    plan: &InsertPlan,
    batches: &mut mpsc::Receiver<Result<Batch, String>>,
    cancel: &AtomicBool,
    progress: &mut (dyn FnMut(u64) + Send),
) -> Result<u64, String> {
    let mut rows = 0u64;
    while let Some(batch) = batches.recv().await {
        let batch = batch?;
        if cancel.load(Ordering::Relaxed) {
            return Err(IMPORT_CANCELLED.into());
        }
        if batch.is_empty() {
            continue;
        }
        conn.execute(&format!("SAVEPOINT {SAVEPOINT}")).await?;
        if let Err(err) = conn.execute(&plan.statement(batch.iter().map(|(_, values)| values))).await {
            conn.execute(&format!("ROLLBACK TO SAVEPOINT {SAVEPOINT}")).await?;
            return Err(pinpoint(conn, plan, &batch, err).await);
        }
        conn.execute(&format!("RELEASE SAVEPOINT {SAVEPOINT}")).await?;
        rows += batch.len() as u64;
        progress(rows);
    }
    Ok(rows)
}

/**
 * Reads the file on a blocking thread and sends the mapped values in batches.
 * Stops quietly when cancelled or when nobody is receiving any more.
 */
pub(crate) fn parse_batches(
    mut reader: Box<dyn BufRead + Send>,
    spec: SourceSpec,
    mut columns: Vec<String>,
    sources: Vec<usize>,
    cancel: Arc<AtomicBool>,
    tx: mpsc::Sender<Result<Batch, String>>,
) {
    let mut batch: Batch = Vec::new();
    let mut bytes = 0;
    let read = tabular::read_source(&mut *reader, &spec, &mut columns, false, |position, values| {
        if cancel.load(Ordering::Relaxed) {
            return Ok(false);
        }
        let picked: Vec<ImportValue> = sources
            .iter()
            .map(|&source| values.get(source).cloned().unwrap_or(ImportValue::Null))
            .collect();
        bytes += picked.iter().map(ImportValue::size).sum::<usize>();
        batch.push((position, picked));
        if batch.len() >= BATCH_ROWS || bytes >= BATCH_BYTES {
            bytes = 0;
            if tx.blocking_send(Ok(std::mem::take(&mut batch))).is_err() {
                return Ok(false);
            }
        }
        Ok(true)
    });
    let last = read.map(|()| batch);
    if !matches!(&last, Ok(batch) if batch.is_empty()) {
        let _ = tx.blocking_send(last);
    }
}

fn check_names<'a>(names: impl Iterator<Item = &'a str>, what: &str) -> Result<(), String> {
    let mut seen = HashSet::new();
    for name in names {
        if name.trim().is_empty() {
            return Err(format!("Every {what} needs a name."));
        }
        if !seen.insert(name.to_lowercase()) {
            return Err(format!("“{name}” is used for more than one {what}."));
        }
    }
    Ok(())
}

/**
 * Inserts the rows of a CSV or JSON file into a table, creating it first if
 * asked. Every row goes in inside one transaction, so a failure or cancel
 * leaves the table as it was, and a table created for the import is dropped.
 */
#[tauri::command]
pub async fn import_rows(
    app: AppHandle,
    sessions: State<'_, SessionStore>,
    transfers: State<'_, TransferStore>,
    connection_id: String,
    transfer_id: String,
    request: ImportRowsRequest,
) -> Result<ImportRowsResult, String> {
    let session = sessions.get(&connection_id).await?;
    let driver = session.driver;
    let namespace = request.namespace.as_str();
    let table = request.table.trim();
    if table.is_empty() {
        return Err("Choose a table to import into.".into());
    }
    if request.mapping.is_empty() {
        return Err("Choose at least one column to import.".into());
    }
    if let Some(bad) = request.mapping.iter().find(|mapping| mapping.source >= request.source_columns.len()) {
        return Err(format!("Column {} isn't in the file. Load the file again.", bad.source + 1));
    }
    check_names(request.mapping.iter().map(|mapping| mapping.target.as_str()), "imported column")?;
    let qualified = dialect(driver).qualified(namespace, table);
    let target_names: Vec<String> = request.mapping.iter().map(|mapping| mapping.target.clone()).collect();

    let (create_sql, targets) = match &request.create {
        Some(new_table) => {
            if tables_in(session.clone(), namespace).await?.iter().any(|known| known.name == table) {
                return Err(format!("A table named “{table}” already exists. Pick another name, or import into it."));
            }
            check_names(new_table.columns.iter().map(|column| column.name.as_str()), "column")?;
            if let Some(column) = new_table.columns.iter().find(|column| column.data_type.trim().is_empty()) {
                return Err(format!("Choose a type for “{}”.", column.name));
            }
            if new_table.id_column && new_table.columns.iter().any(|column| column.name.eq_ignore_ascii_case("id")) {
                return Err("The file already has an id column. Rename it, or turn off the added id column.".into());
            }
            let targets = target_names
                .iter()
                .map(|name| {
                    new_table
                        .columns
                        .iter()
                        .find(|column| &column.name == name)
                        .map(|column| TargetColumn::of(&column.data_type))
                        .ok_or_else(|| format!("“{name}” isn't one of the new table's columns."))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let columns: Vec<(String, String)> = new_table
                .columns
                .iter()
                .map(|column| (column.name.clone(), column.data_type.clone()))
                .collect();
            (Some(tabular::create_table_sql(driver, &qualified, &columns, new_table.id_column)), targets)
        }
        None => {
            let columns = table_columns(&session, namespace, table).await?;
            if columns.is_empty() {
                return Err(format!("“{table}” no longer exists. Refresh the table list and try again."));
            }
            let targets = target_names
                .iter()
                .map(|name| {
                    columns
                        .iter()
                        .find(|column| &column.name == name)
                        .map(|column| TargetColumn::of(&column.data_type))
                        .ok_or_else(|| format!("“{name}” isn't a column of “{table}”. Refresh the table and try again."))
                })
                .collect::<Result<Vec<_>, _>>()?;
            (None, targets)
        }
    };
    let sequences = if driver == Driver::Postgres && create_sql.is_none() {
        let output = session.pool.run(&postgres::sequence_columns_sql(namespace, table), usize::MAX).await?;
        postgres::sequence_columns(&output)
            .into_iter()
            .filter(|column| target_names.contains(&column.name))
            .collect()
    } else {
        Vec::new()
    };
    let overriding = sequences.iter().any(|column| column.always);
    let (prefix, suffix) = tabular::insert_parts(driver, &qualified, &target_names, request.skip_conflicts, overriding);
    let plan = InsertPlan { driver, prefix, suffix, targets };

    let mut file = open_sql_file(&request.path)?;
    let spec = resolve_spec(&request.path, &mut file, &request.file)?;
    let read = file.read.clone();
    let total_bytes = file.total_bytes;
    let file_label = file.name.clone();
    let transfer = transfers.start(&transfer_id);
    let started = Instant::now();
    let (tx, mut rx) = mpsc::channel(4);
    let sources = request.mapping.iter().map(|mapping| mapping.source).collect();
    let parser = {
        let cancel = transfer.cancel.clone();
        let columns = request.source_columns.clone();
        tauri::async_runtime::spawn_blocking(move || parse_batches(file.reader, spec, columns, sources, cancel, tx))
    };

    let mut created = false;
    let outcome = async {
        let mut conn = session.pool.detached().await?;
        let result = async {
            if let Some(sql) = &create_sql {
                conn.execute(sql).await?;
                created = true;
            }
            conn.execute(if driver == Driver::Mysql { "START TRANSACTION" } else { "BEGIN" }).await?;
            let ran = async {
                if request.empty_first && create_sql.is_none() {
                    conn.execute(&format!("DELETE FROM {qualified}")).await?;
                }
                let mut throttle = Throttle(None);
                let mut progress = |rows: u64| {
                    if throttle.ready(false) {
                        let _ = app.emit(
                            IMPORT_PROGRESS_EVENT,
                            ImportRowsProgress {
                                transfer_id: &transfer_id,
                                rows,
                                bytes: read.load(Ordering::Relaxed),
                                total_bytes,
                            },
                        );
                    }
                };
                let rows = insert_batches(&mut conn, &plan, &mut rx, &transfer.cancel, &mut progress).await?;
                if transfer.cancelled() {
                    return Err(IMPORT_CANCELLED.to_string());
                }
                for column in &sequences {
                    conn.execute(&postgres::sync_sequence_statement(&qualified, column).sql).await?;
                }
                conn.execute("COMMIT").await?;
                Ok(rows)
            }
            .await;
            if ran.is_err() {
                let _ = conn.execute("ROLLBACK").await;
            }
            ran
        }
        .await;
        if result.is_err() && created {
            let _ = conn.execute(&format!("DROP TABLE {qualified}")).await;
        }
        conn.close().await;
        result
    }
    .await;
    drop(rx);
    let _ = parser.await;
    if created {
        session.forget_columns(Some(namespace), Some(table));
    }

    let outcome = outcome.map_err(|err| match (err == IMPORT_CANCELLED, created) {
        (true, _) => err,
        (false, true) => format!("{err}\n\nThe import was rolled back and the new table was removed, so nothing changed."),
        (false, false) => format!("{err}\n\nThe import was rolled back, so nothing changed."),
    });
    let sql = match &outcome {
        Ok(rows) => format!("-- Imported {rows} rows from {file_label} into {table}"),
        Err(_) => format!("-- Import from {file_label} into {table}"),
    };
    query_log::record(QueryRecord {
        connection_id: &session.entry.id,
        connection: &session.name,
        driver: driver.label(),
        database: namespace,
        sql: &sql,
        origin: QueryOrigin::Edit,
        duration: started.elapsed(),
        outcome: match &outcome {
            Ok(rows) => Ok(Some(*rows)),
            Err(err) => Err(err.as_str()),
        },
    });
    outcome.map(|rows| ImportRowsResult {
        rows,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        created,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::tabular::JsonStyle;
    use sqlx::Connection;

    async fn sqlite() -> Conn {
        Conn::Sqlite(sqlx::SqliteConnection::connect("sqlite::memory:").await.unwrap())
    }

    async fn export(conn: &mut Conn, sql: &str, options: &TabularOptions) -> String {
        let mut out: Vec<u8> = Vec::new();
        let cancel = AtomicBool::new(false);
        let mut report = |_: u64| {};
        let mut writer = TabularWriter::new(&mut out, options);
        write_query(conn, sql, &[], &mut writer, &cancel, &mut report).await.unwrap();
        writer.finish().unwrap();
        String::from_utf8(out).unwrap()
    }

    async fn import(conn: &mut Conn, text: String, spec: SourceSpec, columns: &[&str], table: &str) -> Result<u64, String> {
        let names: Vec<String> = columns.iter().map(|name| name.to_string()).collect();
        let mut targets = vec![TargetColumn::default(); names.len()];
        if let Some(slot) = names.iter().position(|name| name == "avatar") {
            targets[slot] = TargetColumn::of("BLOB");
        }
        let (prefix, suffix) = tabular::insert_parts(Driver::Sqlite, &format!("\"{table}\""), &names, false, false);
        let plan = InsertPlan { driver: Driver::Sqlite, prefix, suffix, targets };
        let (tx, mut rx) = mpsc::channel(4);
        let cancel = Arc::new(AtomicBool::new(false));
        let sources = (0..names.len()).collect();
        let reader: Box<dyn BufRead + Send> = Box::new(std::io::Cursor::new(text.into_bytes()));
        let parser = tokio::task::spawn_blocking(move || parse_batches(reader, spec, names, sources, cancel, tx));
        conn.execute("BEGIN").await.unwrap();
        let mut progress = |_: u64| {};
        let outcome = insert_batches(conn, &plan, &mut rx, &AtomicBool::new(false), &mut progress).await;
        conn.execute(if outcome.is_ok() { "COMMIT" } else { "ROLLBACK" }).await.unwrap();
        drop(rx);
        parser.await.unwrap();
        outcome
    }

    async fn rows(conn: &mut Conn, table: &str) -> Vec<Vec<Option<String>>> {
        let sql = format!(
            "SELECT id, name, score, hex(avatar), active, typeof(name) FROM \"{table}\" ORDER BY id"
        );
        conn.run(&sql, usize::MAX, None).await.unwrap().text_rows()
    }

    async fn seeded() -> Conn {
        let mut conn = sqlite().await;
        for sql in [
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, score REAL, avatar BLOB, active BOOLEAN)",
            "CREATE TABLE copy (id INTEGER PRIMARY KEY, name TEXT, score REAL, avatar BLOB, active BOOLEAN)",
            "INSERT INTO users VALUES (1, 'o''brien, \"jr\"', 2.5, x'cafe', 1), (2, '', NULL, NULL, 0), \
             (3, NULL, -1, x'', NULL), (4, 'line\nbreak', 1e20, NULL, 1)",
        ] {
            conn.execute(sql).await.unwrap();
        }
        conn
    }

    #[tokio::test]
    async fn csv_round_trips_through_a_table() {
        let mut conn = seeded().await;
        let csv = export(&mut conn, "SELECT * FROM users", &TabularOptions::default()).await;
        assert!(csv.starts_with("id,name,score,avatar,active\n1,\"o'brien, \"\"jr\"\"\",2.5,\\xcafe,1\n2,\"\",,,0\n"), "{csv}");
        let spec = SourceSpec { format: Format::Csv, delimiter: b',', header: true, null_markers: vec![String::new()] };
        let columns = ["id", "name", "score", "avatar", "active"];
        assert_eq!(import(&mut conn, csv, spec, &columns, "copy").await.unwrap(), 4);
        assert_eq!(rows(&mut conn, "copy").await, rows(&mut conn, "users").await);
    }

    #[tokio::test]
    async fn json_lines_round_trip_through_a_table() {
        let mut conn = seeded().await;
        let options = TabularOptions { format: Format::Json, json_style: JsonStyle::Lines, ..TabularOptions::default() };
        let json = export(&mut conn, "SELECT * FROM users", &options).await;
        let spec = SourceSpec { format: Format::Json, delimiter: b',', header: true, null_markers: Vec::new() };
        let columns = ["id", "name", "score", "avatar", "active"];
        assert_eq!(import(&mut conn, json, spec, &columns, "copy").await.unwrap(), 4);
        assert_eq!(rows(&mut conn, "copy").await, rows(&mut conn, "users").await);
    }

    #[tokio::test]
    async fn exports_column_names_for_an_empty_result() {
        let mut conn = seeded().await;
        let csv = export(&mut conn, "SELECT id, name FROM users WHERE id < 0", &TabularOptions::default()).await;
        assert_eq!(csv, "id,name\n");
    }

    #[tokio::test]
    async fn a_failing_row_names_its_line_and_rolls_back() {
        let mut conn = sqlite().await;
        conn.execute("CREATE TABLE strict_t (id INTEGER PRIMARY KEY, name TEXT NOT NULL)").await.unwrap();
        let mut csv = String::from("id,name\n");
        for id in 1..=700 {
            if id == 650 {
                csv.push_str(&format!("{id},\n"));
            } else {
                csv.push_str(&format!("{id},n{id}\n"));
            }
        }
        let spec = SourceSpec { format: Format::Csv, delimiter: b',', header: true, null_markers: vec![String::new()] };
        let err = import(&mut conn, csv, spec, &["id", "name"], "strict_t").await.unwrap_err();
        assert!(err.starts_with("Line 651: "), "{err}");
        let count = conn.run("SELECT COUNT(*) FROM strict_t", 1, None).await.unwrap();
        assert_eq!(crate::db::first_text(&count), "0");
    }

    #[test]
    fn previews_a_file_and_guesses_types() {
        let path = std::env::temp_dir().join(format!("recon-preview-{}.csv", uuid::Uuid::new_v4()));
        fs::write(&path, "id;price;when;flag;note\n1;2.5;2024-01-02;true;a\n2;3;2024-01-03;false;\n").unwrap();
        let options = ImportFileOptions { format: None, delimiter: None, header: true, null_markers: vec![String::new()] };
        let preview = preview_file(Driver::Postgres, path.to_str().unwrap(), &options).unwrap();
        let _ = fs::remove_file(&path);
        assert_eq!(preview.format, Format::Csv);
        assert_eq!(preview.delimiter, ';');
        assert_eq!(preview.columns, vec!["id", "price", "when", "flag", "note"]);
        assert_eq!(preview.types, vec!["BIGINT", "DOUBLE PRECISION", "DATE", "BOOLEAN", "TEXT"]);
        assert_eq!(preview.rows[1][4], None);
        assert_eq!(preview.sampled, 2);
    }

    #[test]
    fn names_table_files_uniquely() {
        let tables = [
            DumpTable { name: "Users".into(), view: false },
            DumpTable { name: "users".into(), view: false },
            DumpTable { name: "a/b".into(), view: true },
        ];
        let options = TabularOptions { gzip: true, ..TabularOptions::default() };
        let files = table_files(Path::new("/tmp/out"), &tables, &options);
        let names: Vec<String> = files.iter().map(|path| file_name(path)).collect();
        assert_eq!(names, vec!["Users.csv.gz", "users-2.csv.gz", "a-b.csv.gz"]);
    }
}
