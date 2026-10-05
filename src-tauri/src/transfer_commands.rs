use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use flate2::bufread::MultiGzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

use crate::db::dump::{self, Dump, DumpOptions, DumpProgress, DumpSummary, DumpTable};
use crate::db::restore::{self, BackupInfo};
use crate::db::sql_split::{is_copy_from_stdin, Splitter};
use crate::db::tabular::TabularOptions;
use crate::db::{describe_error, dialect, first_text, Conn, Session, SessionStore, TableInfo};
use crate::db_commands::{reapply_namespace, tables_in};
use crate::models::Driver;
use crate::query_log::{self, QueryOrigin, QueryRecord};
use crate::tabular_commands;

pub(crate) const EXPORT_PROGRESS_EVENT: &str = "export-progress";
pub(crate) const IMPORT_PROGRESS_EVENT: &str = "import-progress";
const PROGRESS_INTERVAL: Duration = Duration::from_millis(120);
pub(crate) const READ_BUFFER: usize = 256 * 1024;
const COPY_CHUNK: usize = 256 * 1024;
pub(crate) const IMPORT_CANCELLED: &str = "Import cancelled.";
const RESTORE_CANCELLED: &str = "Restore cancelled.";

#[derive(Default)]
pub struct TransferStore {
    flags: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl TransferStore {
    pub(crate) fn start<'a>(&'a self, transfer_id: &'a str) -> Transfer<'a> {
        let cancel = Arc::new(AtomicBool::new(false));
        if let Ok(mut flags) = self.flags.lock() {
            flags.insert(transfer_id.to_string(), cancel.clone());
        }
        Transfer {
            store: self,
            transfer_id,
            cancel,
        }
    }

    fn cancel(&self, transfer_id: &str) {
        if let Some(flag) = self.flags.lock().ok().and_then(|flags| flags.get(transfer_id).cloned()) {
            flag.store(true, Ordering::Relaxed);
        }
    }
}

/// Unregisters the transfer however it ends.
pub(crate) struct Transfer<'a> {
    store: &'a TransferStore,
    pub(crate) transfer_id: &'a str,
    pub(crate) cancel: Arc<AtomicBool>,
}

impl Transfer<'_> {
    pub(crate) fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

impl Drop for Transfer<'_> {
    fn drop(&mut self) {
        if let Ok(mut flags) = self.store.flags.lock() {
            flags.remove(self.transfer_id);
        }
    }
}

/// Emits at most every `PROGRESS_INTERVAL`, unless `force` marks a step worth showing right away.
pub(crate) struct Throttle(pub(crate) Option<Instant>);

impl Throttle {
    pub(crate) fn ready(&mut self, force: bool) -> bool {
        if force || self.0.is_none_or(|last| last.elapsed() >= PROGRESS_INTERVAL) {
            self.0 = Some(Instant::now());
            return true;
        }
        false
    }
}

pub(crate) fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

#[tauri::command]
pub fn cancel_transfer(transfers: State<'_, TransferStore>, transfer_id: String) {
    transfers.cancel(&transfer_id);
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportRequest {
    pub namespace: String,
    /// Every table and view in the namespace when absent.
    #[serde(default)]
    pub tables: Option<Vec<String>>,
    pub path: String,
    pub gzip: bool,
    pub structure: bool,
    pub data: bool,
    pub drop_tables: bool,
    /// Rows as CSV or JSON instead of SQL. `path` is a folder when there's more than one table.
    #[serde(default)]
    pub tabular: Option<TabularOptions>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    #[serde(flatten)]
    pub summary: DumpSummary,
    pub bytes: u64,
    pub path: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExportProgressEvent<'a> {
    pub(crate) transfer_id: &'a str,
    pub(crate) table: &'a str,
    pub(crate) index: usize,
    pub(crate) total: usize,
    pub(crate) rows: u64,
}

pub(crate) enum Sink {
    Plain(BufWriter<File>),
    Gzip(GzEncoder<BufWriter<File>>),
}

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Sink::Plain(out) => out.write(buf),
            Sink::Gzip(out) => out.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Sink::Plain(out) => out.flush(),
            Sink::Gzip(out) => out.flush(),
        }
    }
}

impl Sink {
    pub(crate) fn new(file: File, gzip: bool) -> Self {
        if gzip {
            Sink::Gzip(GzEncoder::new(BufWriter::new(file), Compression::default()))
        } else {
            Sink::Plain(BufWriter::new(file))
        }
    }

    pub(crate) fn finish(self) -> io::Result<()> {
        match self {
            Sink::Plain(mut out) => out.flush(),
            Sink::Gzip(out) => out.finish()?.flush(),
        }
    }
}

/// Builds the text placed before the dump, from the server version and what the dump wrote.
type Header<'a> = &'a (dyn Fn(&str, &DumpSummary) -> String + Sync);

pub(crate) fn sibling(path: &Path, suffix: &str) -> PathBuf {
    path.with_file_name(format!("{}.{suffix}", file_name(path)))
}

/**
 * Puts `header` in front of the finished `body` as `dest`. A gzipped header
 * is its own gzip member, which `MultiGzDecoder` reads straight through, so
 * the body is copied as it is rather than compressed again.
 */
fn prepend_header(dest: &Path, body: &Path, header: &str, gzip: bool) -> Result<(), String> {
    let failed = |err: io::Error| format!("Could not write the backup file: {err}");
    let mut out = BufWriter::new(File::create(dest).map_err(failed)?);
    if gzip {
        let mut encoder = GzEncoder::new(&mut out, Compression::default());
        encoder.write_all(header.as_bytes()).map_err(failed)?;
        encoder.finish().map_err(failed)?;
    } else {
        out.write_all(header.as_bytes()).map_err(failed)?;
    }
    io::copy(&mut File::open(body).map_err(failed)?, &mut out).map_err(failed)?;
    out.flush().map_err(failed)?;
    let _ = fs::remove_file(body);
    Ok(())
}

/**
 * Writes next to the destination and renames on success, so a failed or
 * cancelled dump never leaves a truncated file or clobbers an older one.
 * Returns what was written and the file's final size.
 */
#[allow(clippy::too_many_arguments)]
async fn write_dump(
    app: &AppHandle,
    session: &Session,
    transfer: &Transfer<'_>,
    transfer_id: &str,
    namespace: &str,
    tables: &[DumpTable],
    options: DumpOptions,
    path: &Path,
    gzip: bool,
    header: Option<Header<'_>>,
) -> Result<(DumpSummary, u64), String> {
    let partial = sibling(path, "part");
    let body = if header.is_some() { sibling(path, "body.part") } else { partial.clone() };
    let file = File::create(&body).map_err(|err| format!("Could not create {}: {err}", file_name(path)))?;
    let mut sink = Sink::new(file, gzip);

    let outcome = async {
        let mut conn = session.pool.detached().await?;
        let result = async {
            let version = first_text(&conn.run(dialect(session.driver).version_sql(), 1, None).await?);
            let server = format!("{} {version}", session.driver.label());
            let mut throttle = Throttle(None);
            let mut current = String::new();
            let mut progress = |update: DumpProgress| {
                let changed = update.table != current;
                if throttle.ready(changed) {
                    current = update.table.to_string();
                    let _ = app.emit(
                        EXPORT_PROGRESS_EVENT,
                        ExportProgressEvent {
                            transfer_id,
                            table: update.table,
                            index: update.index,
                            total: update.total,
                            rows: update.rows,
                        },
                    );
                }
            };
            let mut dump = Dump::new(&mut sink, &transfer.cancel, &mut progress);
            dump::run(&mut conn, &server, namespace, tables, options, &mut dump).await?;
            Ok::<_, String>((dump.into_summary(), server))
        }
        .await;
        conn.close().await;
        result
    }
    .await;

    let finished = outcome.and_then(|(summary, server)| {
        sink.finish().map_err(|err| format!("Could not write the export file: {err}"))?;
        if let Some(header) = header {
            prepend_header(&partial, &body, &header(&server, &summary), gzip)?;
        }
        fs::rename(&partial, path).map_err(|err| format!("Could not save {}: {err}", file_name(path)))?;
        Ok(summary)
    });
    match finished {
        Ok(summary) => Ok((summary, fs::metadata(path).map(|meta| meta.len()).unwrap_or(0))),
        Err(err) => {
            let _ = fs::remove_file(&partial);
            let _ = fs::remove_file(&body);
            Err(err)
        }
    }
}

#[tauri::command]
pub async fn export_sql(
    app: AppHandle,
    sessions: State<'_, SessionStore>,
    transfers: State<'_, TransferStore>,
    connection_id: String,
    transfer_id: String,
    request: ExportRequest,
) -> Result<ExportResult, String> {
    if request.tabular.is_none() && !request.structure && !request.data {
        return Err("Choose to export the structure, the data, or both.".into());
    }
    let session = sessions.get(&connection_id).await?;
    let listed = tables_in(session.clone(), &request.namespace).await?;
    let tables: Vec<DumpTable> = match &request.tables {
        None => listed.iter().map(dump_table).collect(),
        Some(names) => names
            .iter()
            .map(|name| {
                listed
                    .iter()
                    .find(|table| &table.name == name)
                    .map(dump_table)
                    .ok_or_else(|| format!("{name} no longer exists. Refresh the table list and try again."))
            })
            .collect::<Result<_, _>>()?,
    };
    if tables.is_empty() {
        return Err("There are no tables to export.".into());
    }

    let transfer = transfers.start(&transfer_id);
    if let Some(options) = &request.tabular {
        let path = Path::new(&request.path);
        return tabular_commands::export_tables(&app, &session, &transfer, &request.namespace, &tables, options, path).await;
    }
    let options = DumpOptions {
        structure: request.structure,
        data: request.data,
        drop_tables: request.drop_tables,
        routines: request.tables.is_none(),
    };
    let (summary, bytes) = write_dump(
        &app,
        &session,
        &transfer,
        &transfer_id,
        &request.namespace,
        &tables,
        options,
        Path::new(&request.path),
        request.gzip,
        None,
    )
    .await?;
    Ok(ExportResult { summary, bytes, path: request.path })
}

fn dump_table(table: &TableInfo) -> DumpTable {
    DumpTable { name: table.name.clone(), view: table.kind == "view" }
}

/**
 * The whole namespace with fixed options, gzipped, behind a Recon backup
 * header that records anything the dump had to leave out. Unlike an export,
 * an empty namespace is fine: its routines and types still get backed up.
 */
#[tauri::command]
pub async fn backup_database(
    app: AppHandle,
    sessions: State<'_, SessionStore>,
    transfers: State<'_, TransferStore>,
    connection_id: String,
    transfer_id: String,
    namespace: String,
    path: String,
) -> Result<ExportResult, String> {
    let session = sessions.get(&connection_id).await?;
    let tables: Vec<DumpTable> = tables_in(session.clone(), &namespace).await?.iter().map(dump_table).collect();
    let transfer = transfers.start(&transfer_id);
    let options = DumpOptions { structure: true, data: true, drop_tables: true, routines: true };
    let driver = session.driver;
    let created_at = restore::utc_timestamp();
    let header = |server: &str, summary: &DumpSummary| {
        restore::header(&BackupInfo {
            version: restore::BACKUP_VERSION,
            driver,
            namespace: namespace.clone(),
            server: server.to_string(),
            created_at: created_at.clone(),
            skipped: summary.skipped.clone(),
        })
    };
    let (summary, bytes) = write_dump(
        &app,
        &session,
        &transfer,
        &transfer_id,
        &namespace,
        &tables,
        options,
        Path::new(&path),
        true,
        Some(&header),
    )
    .await?;
    Ok(ExportResult { summary, bytes, path })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    pub statements: u64,
    pub duration_ms: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportProgressEvent<'a> {
    transfer_id: &'a str,
    statements: u64,
    bytes: u64,
    total_bytes: u64,
}

/// Counts bytes read from the file itself, so progress tracks the file size even when it's gzipped.
struct Counting<R> {
    inner: R,
    count: Arc<AtomicU64>,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.count.fetch_add(read as u64, Ordering::Relaxed);
        Ok(read)
    }
}

fn read_line(reader: &mut dyn BufRead, line: &mut Vec<u8>) -> Result<usize, String> {
    line.clear();
    reader
        .read_until(b'\n', line)
        .map_err(|err| format!("Could not read the file: {err}"))
}

/// Streams the data lines after a `COPY ... FROM stdin` up to the closing `\.`.
async fn copy_from_stdin(
    conn: &mut Conn,
    sql: &str,
    reader: &mut (dyn BufRead + Send),
    splitter: &mut Splitter,
    transfer: &Transfer<'_>,
) -> Result<(), String> {
    let Conn::Postgres(conn) = conn else {
        return Err("COPY FROM stdin only works with Postgres.".into());
    };
    let mut copy = conn.copy_in_raw(sql).await.map_err(describe_error)?;
    let mut chunk = Vec::with_capacity(COPY_CHUNK);
    let mut line = Vec::new();
    loop {
        if read_line(reader, &mut line)? == 0 {
            let _ = copy.abort("The file ended inside COPY data.").await;
            return Err("The file ended before the COPY data did.".into());
        }
        splitter.skip_lines(1);
        let content = line.strip_suffix(b"\n").unwrap_or(&line);
        let content = content.strip_suffix(b"\r").unwrap_or(content);
        if content == b"\\." {
            break;
        }
        chunk.extend_from_slice(content);
        chunk.push(b'\n');
        if chunk.len() >= COPY_CHUNK {
            if transfer.cancelled() {
                let _ = copy.abort(IMPORT_CANCELLED).await;
                return Err(IMPORT_CANCELLED.into());
            }
            copy.send(chunk.as_slice()).await.map_err(describe_error)?;
            chunk.clear();
        }
    }
    if !chunk.is_empty() {
        copy.send(chunk.as_slice()).await.map_err(describe_error)?;
    }
    copy.finish().await.map_err(describe_error)?;
    Ok(())
}

/// A .sql or gzipped .sql file, read as plain SQL either way.
pub(crate) struct SqlFile {
    pub(crate) name: String,
    pub(crate) reader: Box<dyn BufRead + Send>,
    pub(crate) read: Arc<AtomicU64>,
    pub(crate) total_bytes: u64,
}

pub(crate) fn open_sql_file(path: &str) -> Result<SqlFile, String> {
    let name = file_name(Path::new(path));
    let file = File::open(path).map_err(|err| format!("Could not open {name}: {err}"))?;
    let total_bytes = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let read = Arc::new(AtomicU64::new(0));
    let mut counted = BufReader::with_capacity(READ_BUFFER, Counting { inner: file, count: read.clone() });
    let gzipped = counted
        .fill_buf()
        .map_err(|err| format!("Could not read {name}: {err}"))?
        .starts_with(&[0x1f, 0x8b]);
    let reader: Box<dyn BufRead + Send> = if gzipped {
        Box::new(BufReader::with_capacity(READ_BUFFER, MultiGzDecoder::new(counted)))
    } else {
        Box::new(counted)
    };
    Ok(SqlFile { name, reader, read, total_bytes })
}

/// Runs every statement in `file` on `conn`, counting them in `executed`, and stops at the first error.
async fn run_sql_file(
    app: &AppHandle,
    conn: &mut Conn,
    driver: Driver,
    transfer: &Transfer<'_>,
    file: &mut SqlFile,
    executed: &mut u64,
) -> Result<(), String> {
    let mut splitter = Splitter::new(driver);
    let mut pending = Vec::new();
    let mut line = Vec::new();
    let mut throttle = Throttle(None);
    loop {
        let size = read_line(&mut *file.reader, &mut line)?;
        if size == 0 {
            splitter.finish(&mut pending);
        } else {
            splitter.push_bytes(&line, &mut pending)?;
        }
        for statement in std::mem::take(&mut pending) {
            if transfer.cancelled() {
                return Err(IMPORT_CANCELLED.to_string());
            }
            let ran = if driver == Driver::Postgres && is_copy_from_stdin(&statement.sql) {
                copy_from_stdin(conn, &statement.sql, &mut *file.reader, &mut splitter, transfer).await
            } else {
                conn.execute(&statement.sql).await.map(|_| ())
            };
            ran.map_err(|err| format!("Line {}: {err}", statement.line))?;
            *executed += 1;
            if throttle.ready(false) {
                let _ = app.emit(
                    IMPORT_PROGRESS_EVENT,
                    ImportProgressEvent {
                        transfer_id: transfer.transfer_id,
                        statements: *executed,
                        bytes: file.read.load(Ordering::Relaxed),
                        total_bytes: file.total_bytes,
                    },
                );
            }
        }
        if size == 0 {
            return Ok(());
        }
    }
}

fn with_statements_run(err: String, executed: u64) -> String {
    match executed {
        0 => err,
        1 => format!("{err}\n\n1 statement before it had already run."),
        count => format!("{err}\n\n{count} statements before it had already run."),
    }
}

/// Logs a file run to the query history and turns it into the command's result.
fn finish_file_run(
    session: &Session,
    namespace: &str,
    sql: &str,
    started: Instant,
    executed: u64,
    outcome: Result<(), String>,
) -> Result<ImportResult, String> {
    query_log::record(QueryRecord {
        connection_id: &session.entry.id,
        connection: &session.name,
        driver: session.driver.label(),
        database: namespace,
        sql,
        origin: QueryOrigin::Edit,
        duration: started.elapsed(),
        outcome: match &outcome {
            Ok(()) => Ok(Some(executed)),
            Err(err) => Err(err.as_str()),
        },
    });
    outcome.map(|()| ImportResult {
        statements: executed,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    })
}

/**
 * Runs a .sql or gzipped .sql file statement by statement on its own
 * connection, stopping at the first error. Nothing is wrapped in a
 * transaction, so statements before a failure stay applied.
 */
#[tauri::command]
pub async fn import_sql(
    app: AppHandle,
    sessions: State<'_, SessionStore>,
    transfers: State<'_, TransferStore>,
    connection_id: String,
    transfer_id: String,
    namespace: String,
    path: String,
) -> Result<ImportResult, String> {
    let session = sessions.get(&connection_id).await?;
    let mut file = open_sql_file(&path)?;
    let transfer = transfers.start(&transfer_id);
    let driver = session.driver;
    let started = Instant::now();
    let mut executed = 0u64;

    let outcome = async {
        let mut conn = session.pool.detached().await?;
        let result = async {
            if let Some(sql) = dialect(driver).use_namespace_sql(&namespace).filter(|_| !namespace.is_empty()) {
                conn.execute(&sql).await?;
            }
            run_sql_file(&app, &mut conn, driver, &transfer, &mut file, &mut executed).await
        }
        .await;
        conn.close().await;
        result
    }
    .await;

    let outcome = outcome.map_err(|err| with_statements_run(err, executed));
    let sql = format!("-- Imported {} ({executed} statements)", file.name);
    finish_file_run(&session, &namespace, &sql, started, executed, outcome)
}

#[tauri::command]
pub fn read_backup_info(path: String) -> Result<BackupInfo, String> {
    let mut file = open_sql_file(&path)?;
    restore::parse_header(&mut *file.reader)
}

/**
 * Clears the namespace and replays a backup into it on one connection. On
 * Postgres both happen in a single transaction, so a failure or cancel leaves
 * everything as it was. Elsewhere DDL commits as it goes, so cancelling is
 * ignored while clearing, where stopping would leave the namespace empty.
 */
#[tauri::command]
pub async fn restore_database(
    app: AppHandle,
    sessions: State<'_, SessionStore>,
    transfers: State<'_, TransferStore>,
    connection_id: String,
    transfer_id: String,
    namespace: String,
    path: String,
) -> Result<ImportResult, String> {
    let session = sessions.get(&connection_id).await?;
    let driver = session.driver;
    let info = read_backup_info(path.clone())?;
    if info.driver != driver {
        return Err(format!(
            "This backup is from a {} database and can't be restored into {}.",
            info.driver.label(),
            driver.label()
        ));
    }
    if namespace.is_empty() && driver != Driver::Sqlite {
        return Err("Choose a database to restore into.".into());
    }
    let mut file = open_sql_file(&path)?;
    let transfer = transfers.start(&transfer_id);
    let started = Instant::now();
    let mut executed = 0u64;

    let renamed = |err: String| if err == IMPORT_CANCELLED { RESTORE_CANCELLED.to_string() } else { err };
    let outcome = async {
        let mut conn = session.pool.detached().await?;
        let result = if driver == Driver::Postgres {
            let ran = async {
                conn.execute("BEGIN").await?;
                restore::reset_namespace(&mut conn, driver, &namespace).await?;
                run_sql_file(&app, &mut conn, driver, &transfer, &mut file, &mut executed).await?;
                if transfer.cancelled() {
                    return Err(IMPORT_CANCELLED.to_string());
                }
                conn.execute("COMMIT").await.map(|_| ())
            }
            .await;
            if ran.is_err() {
                let _ = conn.execute("ROLLBACK").await;
            }
            ran.map_err(|err| format!("{}\n\nThe restore was rolled back, so nothing changed.", renamed(err)))
        } else {
            let ran = async {
                restore::reset_namespace(&mut conn, driver, &namespace).await?;
                transfer.cancel.store(false, Ordering::Relaxed);
                run_sql_file(&app, &mut conn, driver, &transfer, &mut file, &mut executed).await
            }
            .await;
            ran.map_err(|err| with_statements_run(renamed(err), executed))
        };
        conn.close().await;
        result
    }
    .await;

    if outcome.is_ok() {
        let _ = reapply_namespace(&session).await;
    }
    let sql = format!("-- Restored {} ({executed} statements)", file.name);
    finish_file_run(&session, &namespace, &sql, started, executed, outcome)
}

