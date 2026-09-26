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
use crate::db::sql_split::{is_copy_from_stdin, Splitter};
use crate::db::{describe_error, dialect, first_text, Conn, SessionStore};
use crate::db_commands::tables_in;
use crate::models::Driver;
use crate::query_log::{self, QueryOrigin, QueryRecord};

const EXPORT_PROGRESS_EVENT: &str = "export-progress";
const IMPORT_PROGRESS_EVENT: &str = "import-progress";
const PROGRESS_INTERVAL: Duration = Duration::from_millis(120);
const READ_BUFFER: usize = 256 * 1024;
const COPY_CHUNK: usize = 256 * 1024;
const IMPORT_CANCELLED: &str = "Import cancelled.";

#[derive(Default)]
pub struct TransferStore {
    flags: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl TransferStore {
    fn start<'a>(&'a self, transfer_id: &'a str) -> Transfer<'a> {
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
struct Transfer<'a> {
    store: &'a TransferStore,
    transfer_id: &'a str,
    cancel: Arc<AtomicBool>,
}

impl Transfer<'_> {
    fn cancelled(&self) -> bool {
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
struct Throttle(Option<Instant>);

impl Throttle {
    fn ready(&mut self, force: bool) -> bool {
        if force || self.0.is_none_or(|last| last.elapsed() >= PROGRESS_INTERVAL) {
            self.0 = Some(Instant::now());
            return true;
        }
        false
    }
}

fn file_name(path: &Path) -> String {
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
struct ExportProgressEvent<'a> {
    transfer_id: &'a str,
    table: &'a str,
    index: usize,
    total: usize,
    rows: u64,
}

enum Sink {
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
    fn finish(self) -> io::Result<()> {
        match self {
            Sink::Plain(mut out) => out.flush(),
            Sink::Gzip(out) => out.finish()?.flush(),
        }
    }
}

/**
 * Writes next to the destination and renames on success, so a failed or
 * cancelled export never leaves a truncated file or clobbers an older one.
 */
#[tauri::command]
pub async fn export_sql(
    app: AppHandle,
    sessions: State<'_, SessionStore>,
    transfers: State<'_, TransferStore>,
    connection_id: String,
    transfer_id: String,
    request: ExportRequest,
) -> Result<ExportResult, String> {
    if !request.structure && !request.data {
        return Err("Choose to export the structure, the data, or both.".into());
    }
    let session = sessions.get(&connection_id).await?;
    let listed = tables_in(session.clone(), &request.namespace).await?;
    let tables: Vec<DumpTable> = match &request.tables {
        None => listed
            .iter()
            .map(|table| DumpTable { name: table.name.clone(), view: table.kind == "view" })
            .collect(),
        Some(names) => names
            .iter()
            .map(|name| {
                listed
                    .iter()
                    .find(|table| &table.name == name)
                    .map(|table| DumpTable { name: table.name.clone(), view: table.kind == "view" })
                    .ok_or_else(|| format!("{name} no longer exists. Refresh the table list and try again."))
            })
            .collect::<Result<_, _>>()?,
    };
    if tables.is_empty() {
        return Err("There are no tables to export.".into());
    }

    let path = PathBuf::from(&request.path);
    let partial = path.with_file_name(format!("{}.part", file_name(&path)));
    let file = File::create(&partial).map_err(|err| format!("Could not create {}: {err}", file_name(&path)))?;
    let mut sink = if request.gzip {
        Sink::Gzip(GzEncoder::new(BufWriter::new(file), Compression::default()))
    } else {
        Sink::Plain(BufWriter::new(file))
    };
    let transfer = transfers.start(&transfer_id);
    let options = DumpOptions {
        structure: request.structure,
        data: request.data,
        drop_tables: request.drop_tables,
    };

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
                            transfer_id: &transfer_id,
                            table: update.table,
                            index: update.index,
                            total: update.total,
                            rows: update.rows,
                        },
                    );
                }
            };
            let mut dump = Dump::new(&mut sink, &transfer.cancel, &mut progress);
            dump::run(&mut conn, &server, &request.namespace, &tables, options, &mut dump).await?;
            Ok::<_, String>(dump.into_summary())
        }
        .await;
        conn.close().await;
        result
    }
    .await;

    let finished = outcome.and_then(|summary| {
        sink.finish().map_err(|err| format!("Could not write the export file: {err}"))?;
        fs::rename(&partial, &path).map_err(|err| format!("Could not save {}: {err}", file_name(&path)))?;
        Ok(summary)
    });
    match finished {
        Ok(summary) => Ok(ExportResult {
            summary,
            bytes: fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0),
            path: request.path,
        }),
        Err(err) => {
            let _ = fs::remove_file(&partial);
            Err(err)
        }
    }
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
    let name = file_name(Path::new(&path));
    let file = File::open(&path).map_err(|err| format!("Could not open {name}: {err}"))?;
    let total_bytes = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let read = Arc::new(AtomicU64::new(0));
    let mut counted = BufReader::with_capacity(READ_BUFFER, Counting { inner: file, count: read.clone() });
    let gzipped = counted
        .fill_buf()
        .map_err(|err| format!("Could not read {name}: {err}"))?
        .starts_with(&[0x1f, 0x8b]);
    let mut reader: Box<dyn BufRead + Send> = if gzipped {
        Box::new(BufReader::with_capacity(READ_BUFFER, MultiGzDecoder::new(counted)))
    } else {
        Box::new(counted)
    };
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
            let mut splitter = Splitter::new(driver);
            let mut pending = Vec::new();
            let mut line = Vec::new();
            let mut throttle = Throttle(None);
            loop {
                let size = read_line(&mut *reader, &mut line)?;
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
                        copy_from_stdin(&mut conn, &statement.sql, &mut *reader, &mut splitter, &transfer).await
                    } else {
                        conn.execute(&statement.sql).await.map(|_| ())
                    };
                    ran.map_err(|err| format!("Line {}: {err}", statement.line))?;
                    executed += 1;
                    if throttle.ready(false) {
                        let _ = app.emit(
                            IMPORT_PROGRESS_EVENT,
                            ImportProgressEvent {
                                transfer_id: &transfer_id,
                                statements: executed,
                                bytes: read.load(Ordering::Relaxed),
                                total_bytes,
                            },
                        );
                    }
                }
                if size == 0 {
                    return Ok(());
                }
            }
        }
        .await;
        conn.close().await;
        result
    }
    .await;

    let outcome = outcome.map_err(|err| match executed {
        0 => err,
        1 => format!("{err}\n\n1 statement before it had already run."),
        count => format!("{err}\n\n{count} statements before it had already run."),
    });
    query_log::record(QueryRecord {
        connection_id: &session.entry.id,
        connection: &session.name,
        driver: driver.label(),
        database: &namespace,
        sql: &format!("-- Imported {name} ({executed} statements)"),
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
