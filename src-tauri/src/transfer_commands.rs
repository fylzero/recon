use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, UNIX_EPOCH};

use flate2::bufread::MultiGzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::commands::AppState;
use crate::db::dump::{self, Dump, DumpOptions, DumpProgress, DumpSummary, DumpTable};
use crate::db::restore::{self, BackupInfo};
use crate::db::resume::{self, Stop, Tracker};
use crate::db::sql_load::{
    load_sql, starts_like_dump, LoadOptions, LoadSqlError, LoadTrack, LoadWork, LoadWorkerState,
    NOT_A_DUMP,
};
use crate::db::sql_split::{is_copy_from_stdin, Splitter};
use crate::db::tabular::TabularOptions;
use crate::db::{
    self, describe_error, dialect, first_text, quote_literal, text_at, Conn, Session, SessionStore,
    TableInfo,
};
use crate::db_commands::{reapply_namespace, reconnect_in_background, tables_in, Reconnected};
use crate::models::Driver;
use crate::query_log::{self, QueryOrigin, QueryRecord};
use crate::tabular_commands;

pub(crate) const EXPORT_PROGRESS_EVENT: &str = "export-progress";
pub(crate) const IMPORT_PROGRESS_EVENT: &str = "import-progress";
const PROGRESS_INTERVAL: Duration = Duration::from_millis(120);
pub(crate) const READ_BUFFER: usize = 1024 * 1024;
const COPY_CHUNK: usize = 1024 * 1024;
pub(crate) const IMPORT_CANCELLED: &str = "Import cancelled.";
const RESTORE_CANCELLED: &str = "Restore cancelled.";
/// How long an import waits for a dropped connection to come back before stopping to let the user resume later.
const RECONNECT_FOR: Duration = Duration::from_secs(10 * 60);
const RECONNECT_DELAYS: [Duration; 6] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(20),
    Duration::from_secs(30),
];
/// Reconnects in a row that drop again before running anything, after which the run stops instead.
const MAX_STALLED: u32 = 3;

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
        if let Some(flag) = self
            .flags
            .lock()
            .ok()
            .and_then(|flags| flags.get(transfer_id).cloned())
        {
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
        if force
            || self
                .0
                .is_none_or(|last| last.elapsed() >= PROGRESS_INTERVAL)
        {
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
    let body = if header.is_some() {
        sibling(path, "body.part")
    } else {
        partial.clone()
    };
    let file = File::create(&body)
        .map_err(|err| format!("Could not create {}: {err}", file_name(path)))?;
    let mut sink = Sink::new(file, gzip);

    let outcome = async {
        let mut conn = session.pool.detached().await?;
        let result = async {
            let version = first_text(
                &conn
                    .run(dialect(session.driver).version_sql(), 1, None)
                    .await?,
            );
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
        sink.finish()
            .map_err(|err| format!("Could not write the export file: {err}"))?;
        if let Some(header) = header {
            prepend_header(&partial, &body, &header(&server, &summary), gzip)?;
        }
        fs::rename(&partial, path)
            .map_err(|err| format!("Could not save {}: {err}", file_name(path)))?;
        Ok(summary)
    });
    match finished {
        Ok(summary) => Ok((
            summary,
            fs::metadata(path).map(|meta| meta.len()).unwrap_or(0),
        )),
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
                    .ok_or_else(|| {
                        format!("{name} no longer exists. Refresh the table list and try again.")
                    })
            })
            .collect::<Result<_, _>>()?,
    };
    if tables.is_empty() {
        return Err("There are no tables to export.".into());
    }

    let transfer = transfers.start(&transfer_id);
    if let Some(options) = &request.tabular {
        let path = Path::new(&request.path);
        return tabular_commands::export_tables(
            &app,
            &session,
            &transfer,
            &request.namespace,
            &tables,
            options,
            path,
        )
        .await;
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
    Ok(ExportResult {
        summary,
        bytes,
        path: request.path,
    })
}

fn dump_table(table: &TableInfo) -> DumpTable {
    DumpTable {
        name: table.name.clone(),
        view: table.kind == "view",
    }
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
    let tables: Vec<DumpTable> = tables_in(session.clone(), &namespace)
        .await?
        .iter()
        .map(dump_table)
        .collect();
    let transfer = transfers.start(&transfer_id);
    let options = DumpOptions {
        structure: true,
        data: true,
        drop_tables: true,
        routines: true,
    };
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
    Ok(ExportResult {
        summary,
        bytes,
        path,
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    pub statements: u64,
    pub duration_ms: u64,
}

/// The server connection a run used, so a resumed run can end it in case it's still running the last statement.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Backend {
    pub id: i64,
    /// When the Postgres backend started, so a process id the server has since reused isn't mistaken for it.
    #[serde(default)]
    pub started: Option<String>,
}

/// Where a run that lost its connection can pick up again.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumePoint {
    /// Statements to skip to run the uncertain ones again, or just to carry on when none are.
    pub from: u64,
    /// Statements to skip to go past the uncertain ones instead. Equal to `from` when none are.
    pub to: u64,
    /// The line statement `from` starts on.
    pub line: usize,
    pub fingerprint: String,
    pub backend: Option<Backend>,
    /// Running the uncertain statements again hit rows they add, so they were most likely saved.
    pub likely_saved: bool,
    /// The run loaded the file with `load_sql`, so a resumed run must too, with the same `split_tables`.
    pub parallel: bool,
    pub split_tables: bool,
    /**
     * For a parallel run, the statements that finished, as half-open index
     * ranges. Workers finish out of file order, so `from` alone can't say.
     */
    pub done: Vec<[u64; 2]>,
    /// For a parallel run, the statements between `from` and `to` that may or may not have been saved.
    pub uncertain: Vec<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportError {
    pub message: String,
    pub resume: Option<Box<ResumePoint>>,
}

impl From<String> for ImportError {
    fn from(message: String) -> Self {
        ImportError {
            message,
            resume: None,
        }
    }
}

/// Where the frontend asks a run to pick up, taken from a `ResumePoint`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Resume {
    pub skip: u64,
    pub line: usize,
    pub fingerprint: String,
    #[serde(default)]
    pub backend: Option<Backend>,
    #[serde(default)]
    pub parallel: bool,
    #[serde(default)]
    pub split_tables: bool,
    /// `ResumePoint::done`.
    #[serde(default)]
    pub done: Vec<[u64; 2]>,
    /// The uncertain statements to treat as saved, when the user chose to skip past them.
    #[serde(default)]
    pub skipped: Vec<u64>,
}

/// How far a file run has got.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
    statements: u64,
    bytes: u64,
    total_bytes: u64,
    /**
     * `planning` while the file is split, `running`, `skipping` past statements that already
     * ran, or `reconnecting`.
     */
    phase: &'static str,
    file_bytes: u64,
    file_total_bytes: u64,
    /// 0 until the split has counted statements.
    total_statements: u64,
    workers: Vec<WorkerProgress>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkerProgress {
    index: usize,
    statements: u64,
    total_statements: u64,
    /// `waiting`, `running`, or `done`.
    state: &'static str,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportProgressEvent<'a> {
    transfer_id: &'a str,
    #[serde(flatten)]
    progress: Progress,
    attempt: u32,
    error: Option<&'a str>,
    /// Sent once, when a run that could have loaded in parallel settles on file order.
    #[serde(skip_serializing_if = "Option::is_none")]
    in_order: Option<&'a str>,
}

fn emit_progress(
    app: &AppHandle,
    transfer_id: &str,
    progress: Progress,
    attempt: u32,
    error: Option<&str>,
) {
    let _ = app.emit(
        IMPORT_PROGRESS_EVENT,
        ImportProgressEvent {
            transfer_id,
            progress,
            attempt,
            error,
            in_order: None,
        },
    );
}

fn emit_in_order(app: &AppHandle, transfer_id: &str, progress: Progress, why: &str) {
    let _ = app.emit(
        IMPORT_PROGRESS_EVENT,
        ImportProgressEvent {
            transfer_id,
            progress,
            attempt: 0,
            error: None,
            in_order: Some(why),
        },
    );
}

fn idle_workers(workers: usize) -> Vec<WorkerProgress> {
    (0..workers.max(1))
        .map(|index| WorkerProgress {
            index,
            statements: 0,
            total_statements: 0,
            state: LoadWorkerState::Waiting.as_str(),
        })
        .collect()
}

fn worker_rows(work: &LoadWork) -> Vec<WorkerProgress> {
    work.workers
        .iter()
        .map(|worker| WorkerProgress {
            index: worker.index,
            statements: worker.statements,
            total_statements: worker.total_statements,
            state: worker.state.as_str(),
        })
        .collect()
}

struct ProgressGate {
    planning: bool,
    inserts_done: bool,
    throttle: Throttle,
}

struct PlanningWatch {
    gate: Arc<Mutex<ProgressGate>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl PlanningWatch {
    fn start(
        app: AppHandle,
        transfer_id: String,
        file_read: Arc<AtomicU64>,
        file_total: u64,
        track: LoadTrack,
        gate: Arc<Mutex<ProgressGate>>,
    ) -> Self {
        let thread_gate = gate.clone();
        let thread = std::thread::spawn(move || loop {
            let mut gate = thread_gate.lock().unwrap_or_else(|err| err.into_inner());
            if !gate.planning {
                break;
            }
            if gate.throttle.ready(false) {
                let work = track.snapshot();
                emit_progress(
                    &app,
                    &transfer_id,
                    Progress {
                        statements: work.statements,
                        bytes: 0,
                        total_bytes: 0,
                        phase: "planning",
                        file_bytes: file_read.load(Ordering::Relaxed),
                        file_total_bytes: file_total,
                        total_statements: work.total_statements,
                        workers: worker_rows(&work),
                    },
                    0,
                    None,
                );
            }
            drop(gate);
            std::thread::sleep(Duration::from_millis(40));
        });
        Self {
            gate,
            thread: Some(thread),
        }
    }
}

impl Drop for PlanningWatch {
    fn drop(&mut self) {
        self.gate
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .planning = false;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Counts bytes read from the file itself, so progress tracks the file size even when it's gzipped.
/// Counts the bytes taken from `inner`, not what it has buffered ahead, so progress doesn't jump by a whole buffer.
struct Counting<R> {
    inner: R,
    count: Arc<AtomicU64>,
}

impl<R: BufRead> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.count.fetch_add(read as u64, Ordering::Relaxed);
        Ok(read)
    }
}

impl<R: BufRead> BufRead for Counting<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.inner.fill_buf()
    }

    fn consume(&mut self, amount: usize) {
        self.inner.consume(amount);
        self.count.fetch_add(amount as u64, Ordering::Relaxed);
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

/// Reads past the data lines of a `COPY ... FROM stdin` that already ran.
fn skip_copy_data(reader: &mut dyn BufRead, splitter: &mut Splitter) -> Result<(), String> {
    let mut line = Vec::new();
    loop {
        if read_line(reader, &mut line)? == 0 {
            return Err("The file ended before the COPY data did.".into());
        }
        splitter.skip_lines(1);
        let content = line.strip_suffix(b"\n").unwrap_or(&line);
        let content = content.strip_suffix(b"\r").unwrap_or(content);
        if content == b"\\." {
            return Ok(());
        }
    }
}

/// A .sql or gzipped .sql file, read as plain SQL either way.
pub(crate) struct SqlFile {
    pub(crate) name: String,
    pub(crate) reader: Box<dyn BufRead + Send>,
    pub(crate) read: Arc<AtomicU64>,
    pub(crate) total_bytes: u64,
}

impl SqlFile {
    fn progress(&self, statements: u64, phase: &'static str) -> Progress {
        self.progress_for(statements, phase, 0)
    }

    fn progress_for(&self, statements: u64, phase: &'static str, workers: usize) -> Progress {
        let file_bytes = self.read.load(Ordering::Relaxed);
        Progress {
            statements,
            bytes: file_bytes,
            total_bytes: self.total_bytes,
            phase,
            file_bytes,
            file_total_bytes: self.total_bytes,
            total_statements: 0,
            workers: if workers == 0 {
                Vec::new()
            } else {
                idle_workers(workers)
            },
        }
    }
}

pub(crate) fn open_sql_file(path: &str) -> Result<SqlFile, String> {
    let name = file_name(Path::new(path));
    let file = File::open(path).map_err(|err| format!("Could not open {name}: {err}"))?;
    let total_bytes = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let read = Arc::new(AtomicU64::new(0));
    let mut counted = Counting {
        inner: BufReader::with_capacity(READ_BUFFER, file),
        count: read.clone(),
    };
    let gzipped = counted
        .fill_buf()
        .map_err(|err| format!("Could not read {name}: {err}"))?
        .starts_with(&[0x1f, 0x8b]);
    let reader: Box<dyn BufRead + Send> = if gzipped {
        Box::new(BufReader::with_capacity(
            READ_BUFFER,
            MultiGzDecoder::new(counted),
        ))
    } else {
        Box::new(counted)
    };
    Ok(SqlFile {
        name,
        reader,
        read,
        total_bytes,
    })
}

/// How many connections `load_sql` may use. One per logical CPU core on MySQL and Postgres.
fn sql_workers(driver: Driver) -> usize {
    match driver {
        Driver::Sqlite | Driver::Mssql => 1,
        Driver::Mysql | Driver::Postgres => std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(1),
    }
}

/// The multithreaded-import flag stored on app data. Its default, false, lives on that field.
fn multithreaded_import(app: &AppHandle) -> bool {
    let state = app.state::<AppState>();
    let enabled = match state.data.lock() {
        Ok(data) => data.multithreaded_import,
        Err(poisoned) => poisoned.into_inner().multithreaded_import,
    };
    enabled
}

/**
 * Session setup for each extra worker. `load_sql` runs this on those
 * connections only, not on the coordinator, and then repeats the dump's own
 * session statements, so every connection ends up with the dump's settings.
 */
fn worker_preamble(driver: Driver, namespace: &str) -> Vec<String> {
    (!namespace.is_empty())
        .then(|| dialect(driver).use_namespace_sql(namespace))
        .flatten()
        .into_iter()
        .collect()
}

/// Gives the coordinator, which builds the indexes after the rows, more memory to sort with.
async fn tune_import_connection(conn: &mut Conn, driver: Driver) {
    if driver == Driver::Postgres {
        let _ = conn
            .execute("SET SESSION maintenance_work_mem = '256MB'")
            .await;
    }
}

/// Indexes to pass as `skip_completed`. Session statements are left out so a new connection runs them again.
fn finished_skip(completed: &HashSet<usize>, session_indexes: &HashSet<usize>) -> HashSet<usize> {
    if completed.is_empty() {
        HashSet::new()
    } else {
        completed.difference(session_indexes).copied().collect()
    }
}

/// The source line a `load_sql` failure names, when the message has one.
fn statement_line(message: &str) -> Option<usize> {
    let rest = message.strip_prefix("Line ")?;
    rest.split(':').next()?.trim().parse().ok()
}

/// Changes when the file does, so a resumed run never skips ahead in a different file.
fn fingerprint(path: &str) -> Result<String, String> {
    let meta = fs::metadata(path)
        .map_err(|err| format!("Could not open {}: {err}", file_name(Path::new(path))))?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |time| time.as_nanos());
    Ok(format!("{}:{modified}", meta.len()))
}

enum RunError {
    /// The connection dropped. None when it dropped before the run got back to where it was resuming from.
    Lost(String, Option<Stop>),
    /// Statement `index` failed, or the file couldn't be read there.
    Failed(String, u64),
    /// The uncertain statements a resumed run was asked to check can't safely run again.
    Unsure,
}

impl RunError {
    /// For errors raised before any statement runs, `index` being where the run starts.
    fn before_run(err: String, index: u64) -> Self {
        if db::is_connection_lost(&err) {
            RunError::Lost(err, None)
        } else {
            RunError::Failed(err, index)
        }
    }

    fn into_message(self) -> String {
        match self {
            RunError::Lost(message, _) | RunError::Failed(message, _) => message,
            RunError::Unsure => String::new(),
        }
    }
}

/**
 * Runs every statement in `file` on `conn`, counting them in `executed`, and
 * stops at the first error. The first `skip` statements already ran, so of
 * those only the ones that set up the connection run again, while `tracker`
 * follows them all to know which statements are saved for good.
 */
#[allow(clippy::too_many_arguments)]
async fn run_sql_file(
    report: &mut (dyn FnMut(Progress) + Send),
    conn: &mut Conn,
    driver: Driver,
    transfer: &Transfer<'_>,
    file: &mut SqlFile,
    skip: u64,
    tracker: &mut Tracker,
    executed: &mut u64,
) -> Result<(), RunError> {
    let mut splitter = Splitter::new(driver);
    let mut pending = Vec::new();
    let mut line = Vec::new();
    let mut throttle = Throttle(None);
    *executed = 0;
    loop {
        let size = read_line(&mut *file.reader, &mut line)
            .map_err(|err| RunError::Failed(err, *executed))?;
        if size == 0 {
            splitter.finish(&mut pending);
        } else {
            splitter
                .push_bytes(&line, &mut pending)
                .map_err(|err| RunError::Failed(err, *executed))?;
        }
        for statement in std::mem::take(&mut pending) {
            if transfer.cancelled() {
                return Err(RunError::Failed(IMPORT_CANCELLED.into(), *executed));
            }
            let index = *executed;
            let class = resume::classify(driver, &statement.sql);
            let copy = driver == Driver::Postgres && is_copy_from_stdin(&statement.sql);
            tracker.before(index, statement.line, &class);
            let ran = if index < skip {
                if copy {
                    skip_copy_data(&mut *file.reader, &mut splitter)
                        .map_err(|err| RunError::Failed(err, index))?;
                }
                if class.session && !copy {
                    conn.execute(&statement.sql).await.map(|_| ())
                } else {
                    Ok(())
                }
            } else if copy {
                copy_from_stdin(
                    conn,
                    &statement.sql,
                    &mut *file.reader,
                    &mut splitter,
                    transfer,
                )
                .await
            } else {
                conn.execute(&statement.sql).await.map(|_| ())
            };
            if let Err(err) = ran {
                let message = format!("Line {}: {err}", statement.line);
                return Err(match db::is_connection_lost(&err) {
                    true if index < skip => RunError::Lost(message, None),
                    true => RunError::Lost(message, Some(tracker.stop(index, &class))),
                    false => RunError::Failed(message, index),
                });
            }
            tracker.after(index, statement.line, &class);
            *executed += 1;
            if throttle.ready(false) {
                let phase = if *executed < skip {
                    "skipping"
                } else {
                    "running"
                };
                report(file.progress(*executed, phase));
            }
        }
        if size == 0 {
            return Ok(());
        }
    }
}

async fn backend_of(conn: &mut Conn, driver: Driver) -> Option<Backend> {
    let sql = match driver {
        Driver::Mysql => "SELECT CONNECTION_ID()",
        Driver::Postgres => {
            "SELECT pid, backend_start::text FROM pg_stat_activity WHERE pid = pg_backend_pid()"
        }
        Driver::Sqlite => return None,
        Driver::Mssql => "SELECT @@SPID",
    };
    let row = conn.run(sql, 1, None).await.ok()?.text_rows().pop()?;
    Some(Backend {
        id: text_at(&row, 0).parse().ok()?,
        started: row.get(1).cloned().flatten(),
    })
}

/**
 * Ends the server connection a dropped run was using. The server may not
 * have noticed the drop yet and still be running its last statement, which
 * has to finish or roll back before Recon can tell whether it was saved.
 */
async fn end_backend(conn: &mut Conn, driver: Driver, backend: &Backend) {
    let sql = match driver {
        Driver::Mysql => format!("KILL {}", backend.id),
        Driver::Postgres => format!(
            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
             WHERE pid = {} AND backend_start::text = {}",
            backend.id,
            quote_literal(backend.started.as_deref().unwrap_or_default())
        ),
        Driver::Sqlite => return,
        Driver::Mssql => format!("KILL {}", backend.id),
    };
    let _ = conn.execute(&sql).await;
}

/// Whether the uncertain statements in `stop` would fail on a primary key if they ran twice, rather than add rows twice.
async fn safe_to_repeat(conn: &mut Conn, driver: Driver, stop: &Stop) -> Result<bool, String> {
    if !stop.repeatable {
        return Ok(false);
    }
    for table in &stop.tables {
        let Some(sql) = table.primary_key_sql(driver) else {
            return Ok(false);
        };
        let key: Vec<String> = conn
            .run(&sql, usize::MAX, None)
            .await?
            .text_rows()
            .iter()
            .map(|row| text_at(row, 0))
            .collect();
        if !table.sets_key(driver, &key) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// A primary key or unique violation, which is what a statement that already ran hits when it runs again.
fn likely_saved(message: &str) -> bool {
    message.contains("Duplicate entry") || message.contains("duplicate key value")
}

fn uncertain(stop: &Stop) -> String {
    let (what, them) = if stop.to - stop.from == 1 {
        (format!("the statement on line {} was", stop.line), "it")
    } else {
        (
            format!("the statements from line {} on were", stop.line),
            "them",
        )
    };
    format!(
        "Recon reconnected, but can't tell whether {what} saved before the connection dropped. \
         Running {them} again could add rows twice."
    )
}

/// Aborts the thread that copies a user cancel onto the flag `load_sql` watches.
struct CancelWatch {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for CancelWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

/// Walks the file in the same statement order `load_sql` indexes, including `COPY` data as one statement.
fn visit_statements(
    path: &str,
    driver: Driver,
    mut visit: impl FnMut(usize, &crate::db::sql_split::Statement) -> Result<(), String>,
) -> Result<(), String> {
    let mut file = open_sql_file(path)?;
    let mut splitter = Splitter::new(driver);
    let mut pending = Vec::new();
    let mut line = Vec::new();
    let mut index = 0usize;
    loop {
        let size = read_line(&mut *file.reader, &mut line)?;
        if size == 0 {
            splitter.finish(&mut pending);
        } else {
            splitter.push_bytes(&line, &mut pending)?;
        }
        for statement in std::mem::take(&mut pending) {
            let copy = driver == Driver::Postgres && is_copy_from_stdin(&statement.sql);
            if copy {
                skip_copy_data(&mut *file.reader, &mut splitter)?;
            }
            visit(index, &statement)?;
            index += 1;
        }
        if size == 0 {
            return Ok(());
        }
    }
}

fn session_statement_indexes(path: &str, driver: Driver) -> Result<HashSet<usize>, String> {
    let mut session_indexes = HashSet::new();
    visit_statements(path, driver, |index, statement| {
        if resume::classify(driver, &statement.sql).session {
            session_indexes.insert(index);
        }
        Ok(())
    })?;
    Ok(session_indexes)
}

/**
 * Records a dropped `load_sql` attempt.
 *
 * Each finished statement already committed on its own connection, so
 * finished indexes stay in `completed` and only the ones in flight leave it.
 * Each of those may or may not have committed before the drop, so the stop
 * spans them all. With none in flight, the stop is the first statement that
 * hasn't finished. Session indexes are remembered so the next skip set leaves
 * them out. Returns the stop and the lines of its uncertain statements.
 */
fn note_lost_statements(
    path: &str,
    driver: Driver,
    completed: &mut HashSet<usize>,
    session_indexes: &mut HashSet<usize>,
    in_flight: &[usize],
) -> Result<(Stop, HashSet<usize>), String> {
    for index in in_flight {
        completed.remove(index);
    }
    let lost: HashSet<usize> = in_flight.iter().copied().collect();
    let mut stop: Option<Stop> = None;
    let mut first_pending: Option<Stop> = None;
    let mut retry_lines = HashSet::new();
    visit_statements(path, driver, |index, statement| {
        let class = resume::classify(driver, &statement.sql);
        if class.session {
            session_indexes.insert(index);
        }
        let at = index as u64;
        if lost.contains(&index) {
            let mut tracker = Tracker::new(driver);
            tracker.before(at, statement.line, &class);
            let one = tracker.stop(at, &class);
            retry_lines.insert(statement.line);
            match stop.as_mut() {
                Some(stop) => stop.absorb(one),
                None => stop = Some(one),
            }
        } else if first_pending.is_none() && !completed.contains(&index) {
            first_pending = Some(Stop::at(at, statement.line));
        }
        Ok(())
    })?;
    let stop = stop
        .or(first_pending)
        .unwrap_or_else(|| Stop::at(completed.len() as u64, 0));
    Ok((stop, retry_lines))
}

/// Statement indexes as half-open ranges, for a `ResumePoint`.
fn index_ranges(indexes: &HashSet<usize>) -> Vec<[u64; 2]> {
    let mut sorted: Vec<u64> = indexes.iter().map(|&index| index as u64).collect();
    sorted.sort_unstable();
    let mut ranges: Vec<[u64; 2]> = Vec::new();
    for index in sorted {
        match ranges.last_mut() {
            Some(range) if range[1] == index => range[1] = index + 1,
            _ => ranges.push([index, index + 1]),
        }
    }
    ranges
}

/// The statements a resumed parallel run already has, from its `Resume`.
fn resumed_indexes(resume: &Resume) -> Result<HashSet<usize>, String> {
    let too_many = || String::from("This import has run too many statements to resume.");
    let mut completed = HashSet::new();
    for &[start, end] in &resume.done {
        for index in start..end {
            completed.insert(usize::try_from(index).map_err(|_| too_many())?);
        }
    }
    for &index in &resume.skipped {
        completed.insert(usize::try_from(index).map_err(|_| too_many())?);
    }
    Ok(completed)
}

/// Stops other workers without treating `load_sql`'s own cancel as the user cancelling.
fn bridge_cancel(transfer: &Transfer<'_>) -> (Arc<AtomicBool>, CancelWatch) {
    let gate = Arc::new(AtomicBool::new(transfer.cancelled()));
    let user = transfer.cancel.clone();
    let gate_for_watch = gate.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_watch = stop.clone();
    // OS thread rather than a tokio task: `load_sql` splits the file synchronously and would starve a task on the same runtime.
    let thread = std::thread::spawn(move || loop {
        if user.load(Ordering::Relaxed) {
            gate_for_watch.store(true, Ordering::Relaxed);
            break;
        }
        if gate_for_watch.load(Ordering::Relaxed) || stop_for_watch.load(Ordering::Relaxed) {
            break;
        }
        std::thread::park_timeout(Duration::from_millis(20));
    });
    (
        gate,
        CancelWatch {
            stop,
            thread: Some(thread),
        },
    )
}

#[allow(clippy::too_many_arguments)]
async fn load_dump(
    app: &AppHandle,
    transfer: &Transfer<'_>,
    conn: &mut Conn,
    pool: &db::Pool,
    driver: Driver,
    file: &mut SqlFile,
    options: LoadOptions,
    headroom: Option<&db::RaisedMax>,
    preamble: Vec<String>,
    skip: &HashSet<usize>,
    completed: &mut HashSet<usize>,
    executed: &mut u64,
) -> Result<(), LoadSqlError> {
    let (gate, _watch) = bridge_cancel(transfer);
    let headroom = headroom.cloned();
    let pool = pool.clone();
    let open_worker = move || {
        let pool = pool.clone();
        let headroom = headroom.clone();
        async move {
            match &headroom {
                Some(slot) => slot.detached().await,
                None => pool.detached().await,
            }
        }
    };
    let app = app.clone();
    let transfer_id = transfer.transfer_id.to_string();
    let file_read = file.read.clone();
    let file_total = file.total_bytes;
    let track = LoadTrack::planning(options.workers);
    let progress_gate = Arc::new(Mutex::new(ProgressGate {
        planning: true,
        inserts_done: false,
        throttle: Throttle(None),
    }));
    let _planning = PlanningWatch::start(
        app.clone(),
        transfer_id.clone(),
        file_read.clone(),
        file_total,
        track.clone(),
        progress_gate.clone(),
    );
    let report_track = track.clone();
    load_sql(
        conn,
        driver,
        &mut *file.reader,
        options,
        open_worker,
        gate.as_ref(),
        move |progress| {
            if let Some(index) = progress.statement_index {
                completed.insert(index);
            }
            *executed = progress.statements;
            let mut gate = progress_gate.lock().unwrap_or_else(|err| err.into_inner());
            if gate.planning && progress.statement_index.is_some() {
                return;
            }
            let was_planning = std::mem::replace(&mut gate.planning, false);
            let work = report_track.snapshot();
            let just_finished_inserts = !gate.inserts_done
                && work.total_statements > 0
                && work.statements >= work.total_statements;
            if just_finished_inserts {
                gate.inserts_done = true;
            }
            let finished = progress.total_bytes > 0 && progress.bytes >= progress.total_bytes;
            if gate
                .throttle
                .ready(was_planning || just_finished_inserts || finished)
            {
                let file_bytes = file_read.load(Ordering::Relaxed);
                emit_progress(
                    &app,
                    &transfer_id,
                    Progress {
                        statements: work.statements,
                        bytes: progress.bytes,
                        total_bytes: progress.total_bytes,
                        phase: "running",
                        file_bytes,
                        file_total_bytes: file_total,
                        total_statements: work.total_statements,
                        workers: worker_rows(&work),
                    },
                    0,
                    None,
                );
            }
        },
        preamble,
        skip,
        Some(track),
    )
    .await
}

/// How a run loads its file. A resumed run keeps the plan it stopped with, since the two count progress differently.
enum Plan {
    /// Statement by statement in file order on one connection, so `Stop::from` is where to pick up.
    Sequential,
    /// With `load_sql` over several connections, which finish statements out of order.
    Parallel(Parallel),
}

struct Parallel {
    options: LoadOptions,
    /// Every statement that finished, across attempts.
    completed: HashSet<usize>,
    session_indexes: HashSet<usize>,
    /// The statements the last drop left uncertain.
    uncertain: Vec<u64>,
    /// Lines of those statements, to tell whether a later failure is one of them running again.
    retry_lines: HashSet<usize>,
    /// Pool room for the extra connections.
    headroom: Option<db::RaisedMax>,
}

impl Parallel {
    async fn raise(&mut self, session: &Session) {
        if let Some(old) = self.headroom.take() {
            old.close().await;
        }
        let extras = self.options.workers.saturating_sub(1);
        self.headroom = (extras > 0).then(|| session.pool.raise_max(extras as u32));
    }
}

/**
 * Runs a file on its own connection. When the connection drops, it waits for
 * the connection to come back and picks up where the file stopped. Anything
 * the drop leaves uncertain runs again only when that can't add rows twice.
 * Otherwise the run stops with a `ResumePoint` for the user to decide.
 */
struct Replay<'a> {
    app: &'a AppHandle,
    connection_id: &'a str,
    transfer: &'a Transfer<'a>,
    namespace: &'a str,
    path: &'a str,
    fingerprint: String,
    /// The error to report when the run is cancelled.
    cancelled: &'static str,
}

impl Replay<'_> {
    fn interrupted(
        &self,
        message: String,
        stop: &Stop,
        backend: Option<Backend>,
        plan: &Plan,
    ) -> ImportError {
        let (finished, parallel, split_tables, done, uncertain) = match plan {
            Plan::Sequential => (stop.from, false, false, Vec::new(), Vec::new()),
            Plan::Parallel(run) => (
                run.completed.len() as u64,
                true,
                run.options.split_tables,
                index_ranges(&run.completed),
                run.uncertain.clone(),
            ),
        };
        ImportError {
            message: with_statements_run(message, finished),
            resume: Some(Box::new(ResumePoint {
                from: stop.from,
                to: stop.to,
                line: stop.line,
                fingerprint: self.fingerprint.clone(),
                backend,
                likely_saved: false,
                parallel,
                split_tables,
                done,
                uncertain,
            })),
        }
    }

    fn renamed(&self, message: String) -> String {
        match message.strip_suffix(IMPORT_CANCELLED) {
            Some(prefix) => format!("{prefix}{}", self.cancelled),
            None => message,
        }
    }

    fn progress(&self, file: &SqlFile, statements: u64, plan: &Plan) -> Progress {
        match plan {
            Plan::Sequential => file.progress(statements, "running"),
            Plan::Parallel(run) => file.progress_for(statements, "running", run.options.workers),
        }
    }

    /// Whether `load_sql` may run the file in parallel, telling the dialog why when it may not.
    fn parallel(&self) -> Result<bool, ImportError> {
        let mut file = open_sql_file(self.path)?;
        if starts_like_dump(&mut file.reader)? {
            return Ok(true);
        }
        let progress = file.progress(0, "running");
        emit_in_order(self.app, self.transfer.transfer_id, progress, NOT_A_DUMP);
        Ok(false)
    }

    /// Tries to reconnect with growing pauses, for up to `RECONNECT_FOR`.
    async fn reconnect(
        &self,
        stale: &Arc<Session>,
        progress: Progress,
    ) -> Result<Arc<Session>, String> {
        let started = Instant::now();
        let mut attempt = 0u32;
        let progress = Progress {
            phase: "reconnecting",
            ..progress
        };
        let transfer_id = self.transfer.transfer_id;
        emit_progress(self.app, transfer_id, progress.clone(), 0, None);
        loop {
            if self.transfer.cancelled() {
                return Err(self.cancelled.into());
            }
            let err = match reconnect_in_background(self.app, self.connection_id, stale).await {
                Reconnected::Ready(session) => return Ok(session),
                Reconnected::Closed => return Err("The connection was closed.".into()),
                Reconnected::Failed(err) => err,
            };
            let delay = RECONNECT_DELAYS[(attempt as usize).min(RECONNECT_DELAYS.len() - 1)];
            attempt += 1;
            if started.elapsed() + delay > RECONNECT_FOR {
                return Err(format!(
                    "Recon tried to reconnect for {} minutes, then gave up: {err}",
                    RECONNECT_FOR.as_secs() / 60
                ));
            }
            emit_progress(self.app, transfer_id, progress.clone(), attempt, Some(&err));
            let until = Instant::now() + delay;
            while Instant::now() < until {
                if self.transfer.cancelled() {
                    return Err(self.cancelled.into());
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }

    /**
     * One connection's worth of the run: connects, ends the connection a
     * dropped attempt left behind, checks that the uncertain statements in
     * `check` can run again, and runs the file from statement `skip`, or
     * past the statements a parallel run already finished.
     */
    #[allow(clippy::too_many_arguments)]
    async fn attempt(
        &self,
        session: &Session,
        file: &mut SqlFile,
        plan: &mut Plan,
        skip: u64,
        check: Option<&Stop>,
        reset: &mut bool,
        backend: &mut Option<Backend>,
        executed: &mut u64,
    ) -> Result<(), RunError> {
        let driver = session.driver;
        let mut conn = session
            .pool
            .detached()
            .await
            .map_err(|err| RunError::Lost(err, None))?;
        let result = async {
            if let Some(old) = backend.take() {
                end_backend(&mut conn, driver, &old).await;
            }
            *backend = backend_of(&mut conn, driver).await;
            if let Some(stop) = check {
                let safe = safe_to_repeat(&mut conn, driver, stop)
                    .await
                    .map_err(|err| RunError::before_run(err, skip))?;
                if !safe {
                    return Err(RunError::Unsure);
                }
            }
            if let Some(sql) = dialect(driver)
                .use_namespace_sql(self.namespace)
                .filter(|_| !self.namespace.is_empty())
            {
                conn.execute(&sql)
                    .await
                    .map_err(|err| RunError::before_run(err, skip))?;
            }
            if *reset {
                restore::reset_namespace(&mut conn, driver, self.namespace)
                    .await
                    .map_err(|err| RunError::before_run(err, skip))?;
                *reset = false;
                self.transfer.cancel.store(false, Ordering::Relaxed);
            }
            let run = match plan {
                Plan::Sequential => {
                    let mut tracker = Tracker::new(driver);
                    let transfer_id = self.transfer.transfer_id;
                    let mut report =
                        |progress| emit_progress(self.app, transfer_id, progress, 0, None);
                    return run_sql_file(
                        &mut report,
                        &mut conn,
                        driver,
                        self.transfer,
                        file,
                        skip,
                        &mut tracker,
                        executed,
                    )
                    .await;
                }
                Plan::Parallel(run) => run,
            };
            tune_import_connection(&mut conn, driver).await;
            let finished = finished_skip(&run.completed, &run.session_indexes);
            let loaded = load_dump(
                self.app,
                self.transfer,
                &mut conn,
                &session.pool,
                driver,
                file,
                run.options,
                run.headroom.as_ref(),
                worker_preamble(driver, self.namespace),
                &finished,
                &mut run.completed,
                executed,
            )
            .await;
            match loaded {
                Ok(()) => Ok(()),
                Err(LoadSqlError::Failed(message)) => Err(RunError::Failed(message, *executed)),
                Err(LoadSqlError::LostConnection(lost)) => {
                    let (stop, lines) = note_lost_statements(
                        self.path,
                        driver,
                        &mut run.completed,
                        &mut run.session_indexes,
                        &lost.in_flight,
                    )
                    .map_err(|err| {
                        RunError::Failed(format!("{}\n\n{err}", lost.source), *executed)
                    })?;
                    run.uncertain = lost.in_flight.iter().map(|&index| index as u64).collect();
                    run.retry_lines = lines;
                    let message = if stop.line > 0 {
                        format!("Line {}: {}", stop.line, lost.source)
                    } else {
                        lost.source
                    };
                    Err(RunError::Lost(message, Some(stop)))
                }
            }
        }
        .await;
        conn.close().await;
        result
    }

    /**
     * Runs the file in parallel when `multithreaded` is on and the file allows
     * it, otherwise in file order. With `reset`, empties the namespace first,
     * unless this resumes a run that already did.
     */
    async fn run(
        &self,
        session: &mut Arc<Session>,
        resume: Option<Resume>,
        reset: bool,
        executed: &mut u64,
        multithreaded: bool,
    ) -> Result<(), ImportError> {
        let driver = session.driver;
        let workers = sql_workers(driver);
        let split_tables = match &resume {
            Some(resume) => resume.parallel.then_some(resume.split_tables),
            None if multithreaded && workers > 1 => self.parallel()?.then_some(true),
            None => None,
        };
        let mut plan = match split_tables {
            Some(split_tables) => {
                let completed = match &resume {
                    Some(resume) => resumed_indexes(resume)?,
                    None => HashSet::new(),
                };
                let session_indexes = if completed.is_empty() {
                    HashSet::new()
                } else {
                    session_statement_indexes(self.path, driver)?
                };
                let mut run = Parallel {
                    options: LoadOptions {
                        workers,
                        split_tables,
                    },
                    completed,
                    session_indexes,
                    uncertain: Vec::new(),
                    retry_lines: HashSet::new(),
                    headroom: None,
                };
                run.raise(session).await;
                Plan::Parallel(run)
            }
            None => Plan::Sequential,
        };
        let result = self
            .replay(session, resume, reset, executed, &mut plan)
            .await;
        if let Plan::Parallel(Parallel {
            headroom: Some(headroom),
            ..
        }) = &plan
        {
            headroom.close().await;
        }
        result
    }

    async fn replay(
        &self,
        session: &mut Arc<Session>,
        resume: Option<Resume>,
        mut reset: bool,
        executed: &mut u64,
        plan: &mut Plan,
    ) -> Result<(), ImportError> {
        let resumable = session.driver != Driver::Sqlite;
        let (mut at, mut backend) = match resume {
            Some(resume) => {
                reset = false;
                (Stop::at(resume.skip, resume.line), resume.backend)
            }
            None => (Stop::at(0, 0), None),
        };
        let mut retrying: Option<Stop> = None;
        let mut check: Option<Stop> = None;
        let mut dropped = String::new();
        let mut stalled = 0;
        loop {
            if fingerprint(self.path)? != self.fingerprint {
                return Err(format!(
                    "{} changed since the run started, so Recon can't pick up where it stopped.",
                    file_name(Path::new(self.path))
                )
                .into());
            }
            let mut file = open_sql_file(self.path)?;
            if resumable && session.is_lost() {
                let progress = self.progress(&file, at.from, plan);
                *session = match self.reconnect(session, progress).await {
                    Ok(next) => next,
                    Err(err) => return Err(self.interrupted(err, &at, backend, plan)),
                };
                if let Plan::Parallel(run) = plan {
                    run.raise(session).await;
                }
            }
            let checking = check.take();
            let outcome = self
                .attempt(
                    session,
                    &mut file,
                    plan,
                    at.from,
                    checking.as_ref(),
                    &mut reset,
                    &mut backend,
                    executed,
                )
                .await;
            let (message, stop) = match outcome {
                Ok(()) => return Ok(()),
                Err(RunError::Unsure) => {
                    let message = format!("{dropped}\n\n{}", uncertain(&at));
                    return Err(self.interrupted(message, &at, backend, plan));
                }
                Err(RunError::Failed(message, index)) => {
                    let cancelled = message.contains(IMPORT_CANCELLED);
                    let line = statement_line(&message);
                    let message = self.renamed(message);
                    let hit = |stop: &&Stop| match (&*plan, line) {
                        (Plan::Parallel(run), Some(line)) => run.retry_lines.contains(&line),
                        _ => (stop.from..stop.to).contains(&index),
                    };
                    if !cancelled {
                        if let Some(stop) = retrying.as_ref().filter(hit) {
                            let saved = likely_saved(&message);
                            let reason = if saved {
                                "A row it adds is already there, so it most likely ran before the connection dropped."
                            } else {
                                "It may have already run before the connection dropped."
                            };
                            let message = format!("{message}\n\n{reason}");
                            let mut err = self.interrupted(message, stop, backend, plan);
                            if let Some(point) = err.resume.as_mut() {
                                point.likely_saved = saved;
                            }
                            return Err(err);
                        }
                    }
                    let finished = match plan {
                        Plan::Sequential => index,
                        Plan::Parallel(run) => run.completed.len() as u64,
                    };
                    return Err(with_statements_run(message, finished).into());
                }
                Err(RunError::Lost(message, stop)) => (message, stop),
            };
            if !resumable {
                return Err(with_statements_run(message, *executed).into());
            }
            stalled = if stop.is_some() { 0 } else { stalled + 1 };
            let mut stop = stop.unwrap_or_else(|| at.clone());
            if stalled >= MAX_STALLED {
                return Err(self.interrupted(message, &stop, backend, plan));
            }
            if let Some(mut previous) = retrying.take() {
                if stop.from == previous.from {
                    previous.absorb(stop);
                    stop = previous;
                }
            }
            let progress = self.progress(&file, *executed, plan);
            *session = match self.reconnect(session, progress).await {
                Ok(next) => next,
                Err(err) => {
                    let message = format!("{message}\n\n{err}");
                    return Err(self.interrupted(message, &stop, backend, plan));
                }
            };
            if let Plan::Parallel(run) = plan {
                run.raise(session).await;
            }
            if stop.uncertain() {
                if !stop.repeatable {
                    let message = format!("{message}\n\n{}", uncertain(&stop));
                    return Err(self.interrupted(message, &stop, backend, plan));
                }
                check = Some(stop.clone());
                retrying = Some(stop.clone());
            } else if let Plan::Parallel(run) = plan {
                run.uncertain.clear();
                run.retry_lines.clear();
            }
            dropped = message;
            at = stop;
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
    outcome: Result<(), ImportError>,
) -> Result<ImportResult, ImportError> {
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
            Err(err) => Err(err.message.as_str()),
        },
    });
    outcome.map(|()| ImportResult {
        statements: executed,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    })
}

/**
 * The session to run a file on. A resumed run also accepts one marked lost,
 * which it reconnects before going on.
 */
async fn session_for_run(
    sessions: &SessionStore,
    connection_id: &str,
    resume: Option<&Resume>,
) -> Result<Arc<Session>, ImportError> {
    match sessions.get(connection_id).await {
        Ok(session) => Ok(session),
        Err(err) => match sessions.current(connection_id).await {
            Some(session) if resume.is_some() && session.is_lost() => Ok(session),
            _ => Err(err.into()),
        },
    }
}

fn check_resume(resume: Option<&Resume>, fingerprint: &str, path: &str) -> Result<(), ImportError> {
    match resume {
        Some(resume) if resume.fingerprint != fingerprint => Err(format!(
            "{} changed since the run stopped, so Recon can't pick up where it left off. Start it again instead.",
            file_name(Path::new(path))
        )
        .into()),
        _ => Ok(()),
    }
}

/**
 * Runs a .sql or gzipped .sql file on its own connection, stopping at the
 * first error. Nothing is wrapped in a transaction, so statements before a
 * failure stay applied. If the connection drops, it reconnects and picks up
 * where it stopped, and `resume` picks up a run that stopped for good. With
 * multithreaded import on, a file whose result can't depend on statement
 * order loads over several connections.
 */
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn import_sql(
    app: AppHandle,
    sessions: State<'_, SessionStore>,
    transfers: State<'_, TransferStore>,
    connection_id: String,
    transfer_id: String,
    namespace: String,
    path: String,
    resume: Option<Resume>,
) -> Result<ImportResult, ImportError> {
    let multithreaded = multithreaded_import(&app);
    let mut session = session_for_run(&sessions, &connection_id, resume.as_ref()).await?;
    let fingerprint = fingerprint(&path)?;
    check_resume(resume.as_ref(), &fingerprint, &path)?;
    let transfer = transfers.start(&transfer_id);
    let started = Instant::now();
    let mut executed = 0u64;

    let replay = Replay {
        app: &app,
        connection_id: &connection_id,
        transfer: &transfer,
        namespace: &namespace,
        path: &path,
        fingerprint,
        cancelled: IMPORT_CANCELLED,
    };
    let outcome = replay
        .run(&mut session, resume, false, &mut executed, multithreaded)
        .await;
    let sql = format!(
        "-- Imported {} ({executed} statements)",
        file_name(Path::new(&path))
    );
    finish_file_run(&session, &namespace, &sql, started, executed, outcome)
}

#[tauri::command]
pub fn read_backup_info(path: String) -> Result<BackupInfo, String> {
    let mut file = open_sql_file(&path)?;
    restore::parse_header(&mut *file.reader)
}

/// Clears the namespace and replays the backup in one transaction, so a failure or cancel leaves everything as it was.
async fn restore_in_transaction(
    app: &AppHandle,
    session: &Session,
    transfer: &Transfer<'_>,
    driver: Driver,
    namespace: &str,
    path: &str,
    executed: &mut u64,
) -> Result<(), String> {
    let mut file = open_sql_file(path)?;
    let mut conn = session.pool.detached().await?;
    let ran = async {
        if driver == Driver::Mssql {
            // USE cannot run inside a transaction.
            conn.execute(&format!("USE {}", crate::db::quote_bracket(namespace)))
                .await?;
            conn.execute("BEGIN TRAN").await?;
        } else {
            conn.execute("BEGIN").await?;
        }
        restore::reset_namespace(&mut conn, driver, namespace).await?;
        let mut tracker = Tracker::new(driver);
        let mut report = |progress| emit_progress(app, transfer.transfer_id, progress, 0, None);
        run_sql_file(
            &mut report,
            &mut conn,
            driver,
            transfer,
            &mut file,
            0,
            &mut tracker,
            executed,
        )
        .await
        .map_err(RunError::into_message)?;
        if transfer.cancelled() {
            return Err(IMPORT_CANCELLED.to_string());
        }
        conn.execute("COMMIT").await.map(|_| ())
    }
    .await;
    if ran.is_err() {
        let _ = conn.execute("ROLLBACK").await;
    }
    conn.close().await;
    ran.map_err(|err| {
        let err = if err == IMPORT_CANCELLED {
            RESTORE_CANCELLED.to_string()
        } else {
            err
        };
        format!("{err}\n\nThe restore was rolled back, so nothing changed.")
    })
}

/**
 * Clears the namespace and replays a backup into it. On Postgres and SQL
 * Server both happen in a single transaction on one connection, so a failure
 * or cancel leaves everything as it was. Elsewhere DDL commits as it goes, so
 * cancelling is ignored while clearing, where stopping would leave the
 * namespace empty, and the replay runs like an import does.
 */
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn restore_database(
    app: AppHandle,
    sessions: State<'_, SessionStore>,
    transfers: State<'_, TransferStore>,
    connection_id: String,
    transfer_id: String,
    namespace: String,
    path: String,
    resume: Option<Resume>,
) -> Result<ImportResult, ImportError> {
    let multithreaded = multithreaded_import(&app);
    let mut session = session_for_run(&sessions, &connection_id, resume.as_ref()).await?;
    let driver = session.driver;
    let info = read_backup_info(path.clone())?;
    if info.driver != driver {
        return Err(format!(
            "This backup is from a {} database and can't be restored into {}.",
            info.driver.label(),
            driver.label()
        )
        .into());
    }
    if namespace.is_empty() && driver != Driver::Sqlite {
        return Err(String::from("Choose a database to restore into.").into());
    }
    let fingerprint = fingerprint(&path)?;
    check_resume(resume.as_ref(), &fingerprint, &path)?;
    let transfer = transfers.start(&transfer_id);
    let started = Instant::now();
    let mut executed = 0u64;

    let outcome = if driver == Driver::Postgres || driver == Driver::Mssql {
        restore_in_transaction(
            &app,
            &session,
            &transfer,
            driver,
            &namespace,
            &path,
            &mut executed,
        )
        .await
        .map_err(ImportError::from)
    } else {
        let replay = Replay {
            app: &app,
            connection_id: &connection_id,
            transfer: &transfer,
            namespace: &namespace,
            path: &path,
            fingerprint,
            cancelled: RESTORE_CANCELLED,
        };
        replay
            .run(&mut session, resume, true, &mut executed, multithreaded)
            .await
    };

    if outcome.is_ok() {
        let _ = reapply_namespace(&session).await;
    }
    let sql = format!(
        "-- Restored {} ({executed} statements)",
        file_name(Path::new(&path))
    );
    finish_file_run(&session, &namespace, &sql, started, executed, outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn open_live(driver: &str, port: u16, user: &str, database: &str) -> db::Opened {
        let entry: crate::models::ConnectionEntry = serde_json::from_value(serde_json::json!({
            "id": "live", "name": "live", "driver": driver, "host": "127.0.0.1", "port": port,
            "user": user, "database": database, "filePath": "", "sslMode": "prefer", "headerColor": "",
            "savePassword": false,
        }))
        .unwrap();
        match driver {
            "postgres" => db::postgres::open(&entry, None, &Default::default())
                .await
                .unwrap(),
            _ => db::mysql::open(
                &entry,
                std::env::var("RECON_TEST_MYSQL_PASSWORD").ok().as_deref(),
                &Default::default(),
            )
            .await
            .unwrap(),
        }
    }

    fn write_file(name: &str, sql: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("recon-resume-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        fs::write(&path, sql).unwrap();
        path
    }

    /// Runs the file from statement `skip`, ending its server connection from `killer` after `kill_after`.
    async fn run_file(
        pool: &db::Pool,
        killer: &mut Conn,
        driver: Driver,
        path: &Path,
        skip: u64,
        kill_after: Option<Duration>,
    ) -> Result<(), RunError> {
        let store = TransferStore::default();
        let transfer = store.start("live");
        let mut file = open_sql_file(path.to_str().unwrap()).unwrap();
        let mut conn = pool.detached().await.unwrap();
        let backend = backend_of(&mut conn, driver).await.unwrap();
        let mut tracker = Tracker::new(driver);
        let mut executed = 0;
        let mut report = |_| {};
        let run = run_sql_file(
            &mut report,
            &mut conn,
            driver,
            &transfer,
            &mut file,
            skip,
            &mut tracker,
            &mut executed,
        );
        let kill = async {
            if let Some(after) = kill_after {
                tokio::time::sleep(after).await;
                end_backend(killer, driver, &backend).await;
            }
        };
        let (result, ()) = tokio::join!(run, kill);
        conn.close().await;
        result
    }

    fn lost(result: Result<(), RunError>) -> Stop {
        match result {
            Err(RunError::Lost(_, Some(stop))) => stop,
            Err(RunError::Lost(message, None)) => panic!("lost before resuming: {message}"),
            Err(RunError::Failed(message, index)) => {
                panic!("expected a dropped connection, statement {index} failed: {message}")
            }
            Err(RunError::Unsure) => panic!("unexpected Unsure"),
            Ok(()) => panic!("the run finished before the connection was killed"),
        }
    }

    /**
     * Kills an import's connection mid-statement, then resumes it the way a
     * reconnect does, checking the replayed session settings took effect:
     * `RECON_TEST_MYSQL_USER=root cargo test -- --ignored`.
     */
    #[tokio::test]
    #[ignore]
    async fn live_mysql_resumes_after_the_connection_is_killed() {
        let Ok(user) = std::env::var("RECON_TEST_MYSQL_USER") else {
            return;
        };
        let admin = open_live("mysql", 3306, &user, "").await;
        let mut killer = admin.conn;
        for sql in [
            "DROP DATABASE IF EXISTS recon_resume_probe",
            "CREATE DATABASE recon_resume_probe",
            "CREATE TABLE recon_resume_probe.t (id INT PRIMARY KEY, v INT)",
        ] {
            killer.execute(sql).await.unwrap();
        }
        let probe = open_live("mysql", 3306, &user, "recon_resume_probe").await;
        let path = write_file(
            "mysql.sql",
            "/*!40101 SET NAMES utf8mb4 */;\n\
             SET @OLD_FOREIGN_KEY_CHECKS=@@FOREIGN_KEY_CHECKS, FOREIGN_KEY_CHECKS=0;\n\
             INSERT INTO t VALUES (1, 1);\n\
             INSERT INTO t VALUES (2, 2);\n\
             INSERT INTO t SELECT 3, SLEEP(2);\n\
             INSERT INTO t SELECT 4, @@FOREIGN_KEY_CHECKS;\n",
        );

        let kill = Some(Duration::from_millis(500));
        let stop = lost(run_file(&probe.pool, &mut killer, Driver::Mysql, &path, 0, kill).await);
        assert_eq!((stop.from, stop.to, stop.line), (4, 5, 5));
        let mut conn = probe.pool.detached().await.unwrap();
        assert!(safe_to_repeat(&mut conn, Driver::Mysql, &stop)
            .await
            .unwrap());
        conn.close().await;

        // Killing the connection cuts SLEEP short, and the server still saves the row.
        let retried = run_file(
            &probe.pool,
            &mut killer,
            Driver::Mysql,
            &path,
            stop.from,
            None,
        );
        match retried.await {
            Err(RunError::Failed(message, 4)) => assert!(likely_saved(&message), "{message}"),
            other => panic!(
                "expected the retry to hit the primary key: {}",
                other.err().map(RunError::into_message).unwrap_or_default()
            ),
        }
        let resumed = run_file(
            &probe.pool,
            &mut killer,
            Driver::Mysql,
            &path,
            stop.to,
            None,
        );
        if let Err(err) = resumed.await {
            panic!("resume failed: {}", err.into_message());
        }
        let rows = killer
            .run(
                "SELECT id, v FROM recon_resume_probe.t ORDER BY id",
                10,
                None,
            )
            .await
            .unwrap()
            .text_rows();
        let rows: Vec<String> = rows
            .iter()
            .map(|row| format!("{}={}", text_at(row, 0), text_at(row, 1)))
            .collect();
        assert_eq!(rows, ["1=1", "2=2", "3=1", "4=0"]);

        killer
            .execute("DROP DATABASE recon_resume_probe")
            .await
            .unwrap();
        killer.close().await;
        probe.pool.close().await;
        admin.pool.close().await;
    }

    /**
     * Kills an import's connection inside a transaction, which rolls it back,
     * then resumes from the transaction's start past a COPY that already ran:
     * `RECON_TEST_POSTGRES_PORT=5432 cargo test -- --ignored`.
     */
    #[tokio::test]
    #[ignore]
    async fn live_postgres_resumes_from_the_start_of_a_rolled_back_transaction() {
        let Some(port) = std::env::var("RECON_TEST_POSTGRES_PORT")
            .ok()
            .and_then(|port| port.parse().ok())
        else {
            return;
        };
        let admin = open_live("postgres", port, "postgres", "postgres").await;
        let mut killer = admin.conn;
        killer
            .execute("DROP DATABASE IF EXISTS recon_resume_probe")
            .await
            .unwrap();
        killer
            .execute("CREATE DATABASE recon_resume_probe")
            .await
            .unwrap();
        let probe = open_live("postgres", port, "postgres", "recon_resume_probe").await;
        let mut setup = probe.conn;
        setup.execute("CREATE TABLE log (v int)").await.unwrap();
        let path = write_file(
            "postgres.sql",
            "SET client_encoding = 'UTF8';\n\
             SELECT pg_catalog.set_config('search_path', '', false);\n\
             COPY public.log (v) FROM stdin;\n\
             10\n\
             11\n\
             \\.\n\
             BEGIN;\n\
             INSERT INTO public.log VALUES (1);\n\
             INSERT INTO public.log SELECT 2 FROM pg_sleep(2);\n\
             COMMIT;\n\
             INSERT INTO public.log SELECT CASE WHEN current_setting('search_path') = '' THEN 3 ELSE -3 END;\n",
        );

        let kill = Some(Duration::from_millis(500));
        let stop = lost(run_file(&probe.pool, &mut killer, Driver::Postgres, &path, 0, kill).await);
        assert_eq!((stop.from, stop.to, stop.line), (3, 3, 7));

        let resumed = run_file(
            &probe.pool,
            &mut killer,
            Driver::Postgres,
            &path,
            stop.from,
            None,
        );
        assert!(resumed.await.is_ok());
        let values = setup
            .run("SELECT v FROM log ORDER BY v", 10, None)
            .await
            .unwrap()
            .text_rows();
        let values: Vec<String> = values.iter().map(|row| text_at(row, 0)).collect();
        assert_eq!(values, ["1", "2", "3", "10", "11"]);

        setup.close().await;
        probe.pool.close().await;
        killer
            .execute("DROP DATABASE recon_resume_probe")
            .await
            .unwrap();
        killer.close().await;
        admin.pool.close().await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn bridge_cancel_propagates_while_runtime_thread_is_blocked() {
        let store = TransferStore::default();
        let transfer = store.start("planning-cancel");
        let (gate, _watch) = bridge_cancel(&transfer);

        let canceller = std::thread::spawn({
            let user = transfer.cancel.clone();
            move || {
                std::thread::sleep(Duration::from_millis(30));
                user.store(true, Ordering::Relaxed);
            }
        });

        let started = Instant::now();
        while !gate.load(Ordering::Relaxed) {
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "cancel did not reach the planning gate while the runtime thread was busy"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        canceller.join().unwrap();
    }

    #[test]
    fn sql_workers_follows_logical_cpu_cores() {
        let cores = std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(1);
        assert_eq!(sql_workers(Driver::Mysql), cores);
        assert_eq!(sql_workers(Driver::Postgres), cores);
        assert_eq!(sql_workers(Driver::Sqlite), 1);
        assert_eq!(sql_workers(Driver::Mssql), 1);
    }

    #[test]
    fn finished_statements_round_trip_through_a_resume_point() {
        let done: HashSet<usize> = [0, 1, 2, 5, 7, 8].into_iter().collect();
        let ranges = index_ranges(&done);
        assert_eq!(ranges, vec![[0, 3], [5, 6], [7, 9]]);
        let resume = Resume {
            skip: 3,
            line: 4,
            fingerprint: String::new(),
            backend: None,
            parallel: true,
            split_tables: false,
            done: ranges,
            skipped: vec![4],
        };
        let mut expected = done.clone();
        expected.insert(4);
        assert_eq!(resumed_indexes(&resume).unwrap(), expected);
    }

    #[test]
    fn statements_in_flight_on_several_workers_join_into_one_stop() {
        let path = write_file(
            "in-flight.sql",
            "CREATE TABLE a (id int PRIMARY KEY);\nINSERT INTO a VALUES (1);\nCREATE TABLE b (id int PRIMARY KEY);\nINSERT INTO b VALUES (1);\nINSERT INTO a VALUES (2);\n",
        );
        let path = path.to_str().unwrap();
        let mut completed: HashSet<usize> = [0, 2, 4].into_iter().collect();
        let mut sessions = HashSet::new();
        let (stop, retry_lines) =
            note_lost_statements(path, Driver::Mysql, &mut completed, &mut sessions, &[3, 1])
                .unwrap();
        assert_eq!((stop.from, stop.to, stop.line), (1, 4, 2));
        assert_eq!(retry_lines, [2, 4].into_iter().collect());
        assert_eq!(completed, [0, 2, 4].into_iter().collect());

        // With nothing in flight, the run picks up at the first statement that hasn't finished.
        let mut completed: HashSet<usize> = [0, 1].into_iter().collect();
        let (stop, retry_lines) =
            note_lost_statements(path, Driver::Mysql, &mut completed, &mut sessions, &[]).unwrap();
        assert_eq!((stop.from, stop.to, stop.line), (2, 2, 3));
        assert!(retry_lines.is_empty());
    }
}
