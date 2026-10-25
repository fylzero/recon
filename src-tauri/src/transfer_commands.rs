use std::collections::HashMap;
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
use tauri::{AppHandle, Emitter, State};

use crate::db::dump::{self, Dump, DumpOptions, DumpProgress, DumpSummary, DumpTable};
use crate::db::restore::{self, BackupInfo};
use crate::db::resume::{self, Stop, Tracker};
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
pub(crate) const READ_BUFFER: usize = 256 * 1024;
const COPY_CHUNK: usize = 256 * 1024;
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
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportError {
    pub message: String,
    pub resume: Option<ResumePoint>,
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
}

/// How far a file run has got.
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
    statements: u64,
    bytes: u64,
    total_bytes: u64,
    /// `running`, `skipping` past statements that already ran, or `reconnecting`.
    phase: &'static str,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportProgressEvent<'a> {
    transfer_id: &'a str,
    #[serde(flatten)]
    progress: Progress,
    attempt: u32,
    error: Option<&'a str>,
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
        },
    );
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
        Progress {
            statements,
            bytes: self.read.load(Ordering::Relaxed),
            total_bytes: self.total_bytes,
            phase,
        }
    }
}

pub(crate) fn open_sql_file(path: &str) -> Result<SqlFile, String> {
    let name = file_name(Path::new(path));
    let file = File::open(path).map_err(|err| format!("Could not open {name}: {err}"))?;
    let total_bytes = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let read = Arc::new(AtomicU64::new(0));
    let mut counted = BufReader::with_capacity(
        READ_BUFFER,
        Counting {
            inner: file,
            count: read.clone(),
        },
    );
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
    fn interrupted(&self, message: String, stop: &Stop, backend: Option<Backend>) -> ImportError {
        ImportError {
            message: with_statements_run(message, stop.from),
            resume: Some(ResumePoint {
                from: stop.from,
                to: stop.to,
                line: stop.line,
                fingerprint: self.fingerprint.clone(),
                backend,
                likely_saved: false,
            }),
        }
    }

    fn renamed(&self, message: String) -> String {
        if message == IMPORT_CANCELLED {
            self.cancelled.to_string()
        } else {
            message
        }
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
        emit_progress(self.app, transfer_id, progress, 0, None);
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
            emit_progress(self.app, transfer_id, progress, attempt, Some(&err));
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
     * `check` can run again, and runs the file from statement `skip`.
     */
    #[allow(clippy::too_many_arguments)]
    async fn attempt(
        &self,
        session: &Session,
        file: &mut SqlFile,
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
            let mut tracker = Tracker::new(driver);
            let transfer_id = self.transfer.transfer_id;
            let mut report = |progress| emit_progress(self.app, transfer_id, progress, 0, None);
            run_sql_file(
                &mut report,
                &mut conn,
                driver,
                self.transfer,
                file,
                skip,
                &mut tracker,
                executed,
            )
            .await
        }
        .await;
        conn.close().await;
        result
    }

    /// With `reset`, empties the namespace first, unless this resumes a run that already did.
    async fn run(
        &self,
        session: &mut Arc<Session>,
        resume: Option<Resume>,
        mut reset: bool,
        executed: &mut u64,
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
                *session = self
                    .reconnect(session, file.progress(at.from, "running"))
                    .await
                    .map_err(|err| self.interrupted(err, &at, backend.clone()))?;
            }
            let checking = check.take();
            let outcome = self
                .attempt(
                    session,
                    &mut file,
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
                    return Err(self.interrupted(message, &at, backend));
                }
                Err(RunError::Failed(message, index)) => {
                    let message = self.renamed(message);
                    let retried = |stop: &&Stop| (stop.from..stop.to).contains(&index);
                    if let Some(stop) = retrying.as_ref().filter(retried) {
                        let saved = likely_saved(&message);
                        let reason = if saved {
                            "A row it adds is already there, so it most likely ran before the connection dropped."
                        } else {
                            "It may have already run before the connection dropped."
                        };
                        let mut err =
                            self.interrupted(format!("{message}\n\n{reason}"), stop, backend);
                        if let Some(point) = err.resume.as_mut() {
                            point.likely_saved = saved;
                        }
                        return Err(err);
                    }
                    return Err(with_statements_run(message, index).into());
                }
                Err(RunError::Lost(message, stop)) => (message, stop),
            };
            if !resumable {
                return Err(with_statements_run(message, *executed).into());
            }
            stalled = if stop.is_some() { 0 } else { stalled + 1 };
            let mut stop = stop.unwrap_or_else(|| at.clone());
            if stalled >= MAX_STALLED {
                return Err(self.interrupted(message, &stop, backend));
            }
            if let Some(mut previous) = retrying.take() {
                if stop.from == previous.from {
                    previous.absorb(stop);
                    stop = previous;
                }
            }
            let progress = file.progress(*executed, "running");
            *session = match self.reconnect(session, progress).await {
                Ok(next) => next,
                Err(err) => {
                    return Err(self.interrupted(format!("{message}\n\n{err}"), &stop, backend))
                }
            };
            if stop.uncertain() {
                if !stop.repeatable {
                    let message = format!("{message}\n\n{}", uncertain(&stop));
                    return Err(self.interrupted(message, &stop, backend));
                }
                check = Some(stop.clone());
                retrying = Some(stop.clone());
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
 * Runs a .sql or gzipped .sql file statement by statement on its own
 * connection, stopping at the first error. Nothing is wrapped in a
 * transaction, so statements before a failure stay applied. If the
 * connection drops, it reconnects and picks up where it stopped, and
 * `resume` picks up a run that stopped for good.
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
    let outcome = replay.run(&mut session, resume, false, &mut executed).await;
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
async fn restore_postgres(
    app: &AppHandle,
    session: &Session,
    transfer: &Transfer<'_>,
    namespace: &str,
    path: &str,
    executed: &mut u64,
) -> Result<(), String> {
    let mut file = open_sql_file(path)?;
    let mut conn = session.pool.detached().await?;
    let ran = async {
        conn.execute("BEGIN").await?;
        restore::reset_namespace(&mut conn, Driver::Postgres, namespace).await?;
        let mut tracker = Tracker::new(Driver::Postgres);
        let mut report = |progress| emit_progress(app, transfer.transfer_id, progress, 0, None);
        run_sql_file(
            &mut report,
            &mut conn,
            Driver::Postgres,
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
 * Clears the namespace and replays a backup into it on one connection. On
 * Postgres both happen in a single transaction, so a failure or cancel leaves
 * everything as it was. Elsewhere DDL commits as it goes, so cancelling is
 * ignored while clearing, where stopping would leave the namespace empty, and
 * a dropped connection picks up where it stopped like an import does.
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

    let outcome = if driver == Driver::Postgres {
        restore_postgres(&app, &session, &transfer, &namespace, &path, &mut executed)
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
        replay.run(&mut session, resume, true, &mut executed).await
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
}
