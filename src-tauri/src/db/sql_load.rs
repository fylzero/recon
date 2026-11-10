//! Phased SQL dump loader.
//!
//! A dump is split and classified while it is read, written to temporary chunk
//! files, then executed in three phases. Phase 1 runs session and global setup
//! on the coordinator and parallelizes independent table DDL across worker
//! connections when `workers > 1`. Phase 2 runs `INSERT`, `REPLACE`, and
//! `COPY ... FROM stdin` across the coordinator and any extra connections.
//! Phase 3 runs indexes, constraints, triggers, views, and trailing session
//! statements on the coordinator in file order.
//!
//! That order differs from the file's, so only dumps [`starts_like_dump`]
//! knows load this way. Other files run in file order on one connection.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::future::Future;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::resume::{self, Tx};
use super::sql_split::{is_copy_from_stdin, Splitter, Statement};
use super::{describe_error, is_connection_lost, Conn};
use crate::models::Driver;

const CHUNK_TARGET: u64 = 128 * 1024 * 1024;
const CHUNK_IO_BUFFER: usize = 1024 * 1024;
const COPY_SEND: usize = 1024 * 1024;
const TABLE_CHUNK_BYTES: u64 = 16 * 1024 * 1024;
const TABLE_CHUNK_STMTS: u64 = 2048;
const PHASE1_YIELD_BYTES: u64 = 4 * 1024 * 1024;
const PHASE1_YIELD_STMTS: usize = 2048;
const BATCH_TARGET_BYTES: usize = 256 * 1024;
const BATCH_MAX_STMTS: usize = 128;
const MAGIC: &[u8] = b"RSQL1";
const TAG_SQL: u8 = 1;
const TAG_COPY: u8 = 2;
const TAG_DATA: u8 = 3;
const TAG_END: u8 = 4;

/// Counts for one load. `bytes` and `total_bytes` are finished statement text,
/// not the number of bytes read while splitting the file.
///
/// `statement_index` is `None` on the call made before execution. After a
/// statement finishes, or is skipped because its index is in `skip_completed`,
/// it is that statement's file-order index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadProgress {
    pub statements: u64,
    pub bytes: u64,
    pub total_bytes: u64,
    pub statement_index: Option<usize>,
}

/// One loader thread. `statements` is how many of its assigned statements have finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoadWorker {
    pub(crate) index: usize,
    pub(crate) statements: u64,
    pub(crate) total_statements: u64,
    pub(crate) state: LoadWorkerState,
}

/// `waiting` before that thread has started, `running` while it owns the phase, `done` when its count is finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoadWorkerState {
    Waiting,
    Running,
    Done,
}

impl LoadWorkerState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            LoadWorkerState::Waiting => "waiting",
            LoadWorkerState::Running => "running",
            LoadWorkerState::Done => "done",
        }
    }

    const fn to_u8(self) -> u8 {
        match self {
            LoadWorkerState::Waiting => 0,
            LoadWorkerState::Running => 1,
            LoadWorkerState::Done => 2,
        }
    }

    const fn from_u8(value: u8) -> Self {
        match value {
            1 => LoadWorkerState::Running,
            2 => LoadWorkerState::Done,
            _ => LoadWorkerState::Waiting,
        }
    }
}

/// Per-worker counts beside [`LoadProgress`]. `statements` and `total_statements` track
/// Phase 2 data (`INSERT` / `REPLACE` / `COPY`) statements only, excluding Phase 1 setup
/// and Phase 3 cleanup actions. `total_statements` is 0 until the split counts data statements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoadWork {
    pub(crate) statements: u64,
    pub(crate) total_statements: u64,
    pub(crate) workers: Vec<LoadWorker>,
}

struct WorkerAtomics {
    statements: AtomicU64,
    total_statements: AtomicU64,
    state: AtomicU8,
}

impl WorkerAtomics {
    fn new() -> Self {
        Self {
            statements: AtomicU64::new(0),
            total_statements: AtomicU64::new(0),
            state: AtomicU8::new(LoadWorkerState::Waiting.to_u8()),
        }
    }
}

struct LoadTrackInner {
    statements: AtomicU64,
    total_statements: AtomicU64,
    workers: Vec<WorkerAtomics>,
    /// How many of `workers` got a connection.
    active: AtomicUsize,
}

/// Shared worker counts. The load updates them; the emit site reads [`LoadTrack::snapshot`].
#[derive(Clone)]
pub(crate) struct LoadTrack {
    inner: Arc<LoadTrackInner>,
}

impl LoadTrack {
    pub(crate) fn planning(workers: usize) -> Self {
        let workers: Vec<_> = (0..workers.max(1)).map(|_| WorkerAtomics::new()).collect();
        Self {
            inner: Arc::new(LoadTrackInner {
                statements: AtomicU64::new(0),
                total_statements: AtomicU64::new(0),
                active: AtomicUsize::new(workers.len()),
                workers,
            }),
        }
    }

    /// Leaves out the workers whose connection couldn't open.
    fn limit(&self, connections: usize) {
        self.inner.active.store(
            connections.clamp(1, self.inner.workers.len()),
            Ordering::Relaxed,
        );
    }

    pub(crate) fn snapshot(&self) -> LoadWork {
        let statements = self.inner.statements.load(Ordering::Relaxed);
        let total_statements = self.inner.total_statements.load(Ordering::Relaxed);
        let active = self.inner.active.load(Ordering::Relaxed);
        let workers = self
            .inner
            .workers
            .iter()
            .take(active)
            .enumerate()
            .map(|(index, worker)| LoadWorker {
                index,
                statements: worker.statements.load(Ordering::Relaxed),
                total_statements: worker.total_statements.load(Ordering::Relaxed),
                state: LoadWorkerState::from_u8(worker.state.load(Ordering::Relaxed)),
            })
            .collect();
        LoadWork {
            statements,
            total_statements,
            workers,
        }
    }

    fn plan_data(&self) {
        let n = self.inner.workers.len();
        if n == 0 {
            return;
        }
        self.inner.total_statements.fetch_add(1, Ordering::Relaxed);
    }

    fn plan_pre(&self, index: usize) {
        let Some(worker) = self.inner.workers.get(index) else {
            return;
        };
        worker.total_statements.fetch_add(1, Ordering::Relaxed);
        worker
            .state
            .store(LoadWorkerState::Running.to_u8(), Ordering::Relaxed);
    }

    fn bump_pre(&self, index: usize) {
        let Some(worker) = self.inner.workers.get(index) else {
            return;
        };
        let w_total = worker.total_statements.load(Ordering::Relaxed);
        let w_prev = worker.statements.fetch_add(1, Ordering::Relaxed);
        let w_now = w_prev + 1;
        let state = if w_total > 0 && w_now >= w_total {
            LoadWorkerState::Done
        } else {
            LoadWorkerState::Running
        };
        worker.state.store(state.to_u8(), Ordering::Relaxed);
    }

    fn install(&self, data: &[u64]) {
        let n = self.inner.workers.len();
        let slots = if data.is_empty() {
            0
        } else {
            n.min(data.len())
        };
        let mut data_totals = vec![0u64; n];
        if slots > 0 {
            for (index, count) in data.iter().enumerate() {
                data_totals[index % slots] += *count;
            }
        }
        let total_data: u64 = data_totals.iter().sum();
        for (index, worker) in self.inner.workers.iter().enumerate() {
            let data_count = data_totals.get(index).copied().unwrap_or(0);
            worker.statements.store(0, Ordering::Relaxed);
            worker.total_statements.store(data_count, Ordering::Relaxed);
            let state = if total_data == 0 {
                LoadWorkerState::Done
            } else if data_count > 0 {
                LoadWorkerState::Running
            } else {
                LoadWorkerState::Waiting
            };
            worker.state.store(state.to_u8(), Ordering::Relaxed);
        }
        self.inner.statements.store(0, Ordering::Relaxed);
        self.inner
            .total_statements
            .store(total_data, Ordering::Relaxed);
    }

    fn mark_running(&self, index: usize) {
        let Some(worker) = self.inner.workers.get(index) else {
            return;
        };
        let total = worker.total_statements.load(Ordering::Relaxed);
        let stmts = worker.statements.load(Ordering::Relaxed);
        if total == 0 || stmts >= total {
            return;
        }
        worker
            .state
            .store(LoadWorkerState::Running.to_u8(), Ordering::Relaxed);
    }

    fn bump(&self, index: usize) {
        self.bump_many(index, 1);
    }

    fn bump_many(&self, index: usize, count: u64) {
        if count == 0 {
            return;
        }
        let total = self.inner.total_statements.load(Ordering::Relaxed);
        let prev = self.inner.statements.fetch_add(count, Ordering::Relaxed);
        if total > 0 && prev + count > total {
            self.inner.statements.store(total, Ordering::Relaxed);
        }
        let Some(worker) = self.inner.workers.get(index) else {
            return;
        };
        let w_total = worker.total_statements.load(Ordering::Relaxed);
        let w_prev = worker.statements.fetch_add(count, Ordering::Relaxed);
        let w_now = if w_total > 0 && w_prev + count > w_total {
            worker.statements.store(w_total, Ordering::Relaxed);
            w_total
        } else {
            w_prev + count
        };
        let state = if w_total > 0 && w_now >= w_total {
            LoadWorkerState::Done
        } else {
            LoadWorkerState::Running
        };
        worker.state.store(state.to_u8(), Ordering::Relaxed);
    }

    fn steal_one(&self, thief: usize, victim: usize) {
        self.steal_many(thief, victim, 1);
    }

    fn steal_many(&self, thief: usize, victim: usize, count: u64) {
        if thief == victim || count == 0 {
            return;
        }
        if let Some(from) = self.inner.workers.get(victim) {
            let stmts = from.statements.load(Ordering::Relaxed);
            let mut total = from.total_statements.load(Ordering::Relaxed);
            while total > stmts {
                let sub = count.min(total - stmts);
                match from.total_statements.compare_exchange_weak(
                    total,
                    total - sub,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        total -= sub;
                        break;
                    }
                    Err(actual) => total = actual,
                }
            }
            if total > 0 && from.statements.load(Ordering::Relaxed) >= total {
                from.state
                    .store(LoadWorkerState::Done.to_u8(), Ordering::Relaxed);
            }
        }
        if let Some(to) = self.inner.workers.get(thief) {
            to.total_statements.fetch_add(count, Ordering::Relaxed);
            to.state
                .store(LoadWorkerState::Running.to_u8(), Ordering::Relaxed);
        }
    }

    fn finish_worker(&self, index: usize) {
        let Some(worker) = self.inner.workers.get(index) else {
            return;
        };
        let total = worker.total_statements.load(Ordering::Relaxed);
        let stmts = worker.statements.load(Ordering::Relaxed);
        if total > 0 && stmts >= total {
            worker
                .state
                .store(LoadWorkerState::Done.to_u8(), Ordering::Relaxed);
        }
    }
}

/// A database connection dropped while dump statements were executing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadLostConnection {
    /**
     * 0-based file-order indexes of every statement that was running on a
     * connection when it dropped, across all workers, in ascending order.
     * Empty when the drop came between statements.
     */
    pub in_flight: Vec<usize>,
    /// Driver message from the failed call, unchanged. A lost connection keeps
    /// the `describe_error` text, which starts with the connection-lost prefix.
    pub source: String,
}

/// Failure from [`load_sql`].
///
/// Match [`LoadSqlError::LostConnection`] to resume. Its
/// [`LoadLostConnection::in_flight`] lists the statements that may not have finished.
/// Every other failure is [`LoadSqlError::Failed`], and the string is the same
/// message the loader used to return directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadSqlError {
    Failed(String),
    LostConnection(LoadLostConnection),
}

impl std::fmt::Display for LoadSqlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadSqlError::Failed(message) => f.write_str(message),
            LoadSqlError::LostConnection(lost) => {
                f.write_str(&lost.source)?;
                let indexes: Vec<String> = lost.in_flight.iter().map(usize::to_string).collect();
                match indexes.len() {
                    0 => Ok(()),
                    1 => write!(f, " (statement {})", indexes[0]),
                    _ => write!(f, " (statements {})", indexes.join(", ")),
                }
            }
        }
    }
}

impl LoadSqlError {
    /// Joins a later failure into this one. Lost connections from several workers keep every in-flight statement.
    fn absorb(&mut self, later: LoadSqlError) {
        if let (LoadSqlError::LostConnection(lost), LoadSqlError::LostConnection(more)) =
            (self, later)
        {
            lost.in_flight.extend(more.in_flight);
            lost.in_flight.sort_unstable();
            lost.in_flight.dedup();
        }
    }
}

impl std::error::Error for LoadSqlError {}

impl From<String> for LoadSqlError {
    fn from(message: String) -> Self {
        LoadSqlError::Failed(message)
    }
}

/// How [`load_sql`] spreads the work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LoadOptions {
    /// Connections to use, the coordinator included. Values below 1 run as 1.
    pub(crate) workers: usize,
    /**
     * Whether one table's rows may run on more than one connection at once.
     * Only for dumps whose rows carry their own keys, the only files
     * [`starts_like_dump`] lets through, since rows that take the next auto-increment value, or that
     * replace or ignore each other, depend on running in file order.
     */
    pub(crate) split_tables: bool,
}

/**
 * Loads a SQL dump onto `coordinator` in three phases.
 *
 * `reader` is any buffered reader. Wrap a plain reader in `std::io::BufReader`
 * first. Gzip is the caller's concern; pass the decoded SQL stream.
 *
 * `options.workers` is the parallelism, the coordinator included. When it is
 * 1, every phase uses `coordinator` and `open_worker` is not called. When it
 * is greater than 1, `open_worker` is called `workers - 1` times before
 * anything runs, and the load goes ahead on however many of those opened.
 * Those connections are closed before this function returns. The coordinator
 * stays open. A table's rows stay on one connection, in file order, unless
 * `options.split_tables` is set.
 *
 * `preamble` is split with the same statement splitter and run on each extra
 * worker before anything else. It is not run on the coordinator: the dump's own
 * leading `SET` / `USE` / `PRAGMA` statements are phase 1, and its trailing
 * `SET` statements are phase 3, both on the coordinator. Session statements
 * phase 1 runs are repeated on every extra worker too. Pass an empty iterator
 * for SQLite. Preamble text is not part of the dump, so it has no file-order
 * index. On MySQL, foreign key checks are off on every connection while the
 * rows load, then back to the coordinator's setting for phase 3.
 *
 * `skip_completed` lists file-order indexes that already finished. Those
 * statements are not sent again. Indexes are assigned while the file is split,
 * before anything runs: the first statement the splitter yields is 0, then 1,
 * 2, and so on, in file order, including statements other workers run.
 * `COPY ... FROM stdin` is one index. Its data lines are not statements.
 * `LOCK TABLES` / `UNLOCK TABLES` keep their indexes when `workers > 1` omits
 * them, so a later retry of the same file reproduces the same numbers. The
 * same file always yields the same indexes, whatever `workers` is.
 *
 * `cancel` is checked between statements. The first statement error is returned
 * as [`LoadSqlError::Failed`] with `Line N: ...` (N is the statement's source
 * line), and the flag is set so other workers stop at their next statement.
 * A dropped connection is [`LoadSqlError::LostConnection`] instead:
 * `in_flight` lists every statement that was executing on any connection, and
 * `source` is the driver message. Statements that already finished stay applied.
 *
 * `progress` is called once before execution, with `statements` and `bytes` at
 * 0, `total_bytes` set, and `statement_index` `None`, then again after each
 * finished statement, including one `skip_completed` did not re-run.
 * `statement_index` is that statement's file-order index. Calls are
 * serialized. The callback runs inline; it must not re-enter the load.
 *
 * Temporary chunk files live in the system temp directory and are removed when
 * the load finishes or fails.
 */
#[allow(clippy::too_many_arguments)]
pub(crate) async fn load_sql<R, Open, Fut, Report, Preamble, PreambleSql>(
    coordinator: &mut Conn,
    driver: Driver,
    reader: R,
    options: LoadOptions,
    mut open_worker: Open,
    cancel: &AtomicBool,
    progress: Report,
    preamble: Preamble,
    skip_completed: &HashSet<usize>,
    track: Option<LoadTrack>,
) -> Result<(), LoadSqlError>
where
    R: BufRead + Send,
    Open: FnMut() -> Fut + Send,
    Fut: Future<Output = Result<Conn, String>> + Send,
    Report: FnMut(LoadProgress) + Send,
    Preamble: IntoIterator<Item = PreambleSql>,
    PreambleSql: AsRef<str>,
{
    let workers = if driver == Driver::Sqlite {
        1
    } else {
        options.workers.max(1)
    };
    let preamble = expand_preamble(driver, preamble);
    let extras = if workers > 1 {
        let opened = futures_util::future::join_all((1..workers).map(|_| open_worker())).await;
        opened.into_iter().filter_map(Result::ok).collect()
    } else {
        Vec::new()
    };
    if let Some(track) = &track {
        track.limit(1 + extras.len());
    }
    let mut temps = TempFiles::default();
    let state = Mutex::new(ProgressState {
        statements: 0,
        bytes: 0,
        total_bytes: 0,
        report: progress,
        track: track.clone(),
    });
    let error = Mutex::new(None);
    let ctx = RunCtx {
        preamble: &preamble,
        cancel,
        error: &error,
        progress: &state,
        skip_completed,
        in_tx: AtomicBool::new(false),
    };
    let mut plan = run_phase1(
        coordinator,
        reader,
        driver,
        workers,
        options.split_tables,
        extras,
        &ctx,
        &mut temps,
    )
    .await?;
    if let Some(track) = &track {
        track.install(&plan.data_statements);
    }
    finish_phase1(&state, plan.total_bytes);
    let extras = std::mem::take(&mut plan.extras);
    let foreign_keys = if driver == Driver::Mysql && workers > 1 && !plan.data.is_empty() {
        match pause_foreign_keys(coordinator).await {
            Ok(setting) => Some(setting),
            Err(err) => {
                close_conns(extras).await;
                return Err(err);
            }
        }
    } else {
        None
    };
    let setup: Vec<String> = match foreign_keys {
        Some(_) => vec![FOREIGN_KEYS_OFF.to_string()],
        None => Vec::new(),
    };
    run_phase2(
        coordinator,
        &plan.data,
        extras,
        &setup,
        options.split_tables,
        &ctx,
    )
    .await?;
    if let Some(setting) = foreign_keys {
        coordinator
            .execute(&format!("SET FOREIGN_KEY_CHECKS = {setting}"))
            .await
            .map_err(between_statements)?;
    }
    if !plan.post.is_empty() {
        exec_post(coordinator, driver, workers, &plan.post, &ctx).await;
        take_error(ctx.error, cancel)?;
    }
    drop(temps);
    Ok(())
}

const FOREIGN_KEYS_OFF: &str = "SET FOREIGN_KEY_CHECKS = 0";

/**
 * Turns MySQL foreign key checks off on the coordinator for phase 2, which
 * loads tables in no particular order, and returns the setting to put back.
 */
async fn pause_foreign_keys(conn: &mut Conn) -> Result<&'static str, LoadSqlError> {
    let rows = conn
        .run("SELECT @@SESSION.foreign_key_checks", 1, None)
        .await
        .map_err(between_statements)?
        .text_rows();
    let on = rows
        .first()
        .and_then(|row| row.first().cloned().flatten())
        .is_none_or(|value| value != "0");
    conn.execute(FOREIGN_KEYS_OFF)
        .await
        .map_err(between_statements)?;
    Ok(if on { "1" } else { "0" })
}

/// A setup statement failed on an extra worker.
fn worker_setup_failure(message: String) -> LoadSqlError {
    if is_connection_lost(&message) {
        between_statements(message)
    } else {
        LoadSqlError::Failed(format!("Worker setup failed: {message}"))
    }
}

/// An error from a statement the loader adds itself, which has no index or line in the file.
fn between_statements(message: String) -> LoadSqlError {
    if is_connection_lost(&message) {
        LoadSqlError::LostConnection(LoadLostConnection {
            in_flight: Vec::new(),
            source: message,
        })
    } else {
        LoadSqlError::Failed(message)
    }
}

/// Why a file loads in file order rather than over several connections.
pub(crate) const NOT_A_DUMP: &str = "the file isn't from mysqldump, pg_dump or a Recon export";

/**
 * First lines of the dumps [`load_sql`] may load over several connections.
 * Their tools write rows that carry their own keys and put what depends on
 * rows, like triggers and constraints, after all of them, so the phased load
 * ends with what running them in file order would.
 */
const DUMP_HEADERS: [&str; 5] = [
    "-- MySQL dump",
    "-- MariaDB dump",
    "-- PostgreSQL database dump",
    "-- Recon SQL export",
    "-- Recon backup v",
];

/// Whether the first lines of `reader` name a dump in [`DUMP_HEADERS`], without consuming them.
pub(crate) fn starts_like_dump<R: BufRead>(reader: &mut R) -> Result<bool, String> {
    let head = reader
        .fill_buf()
        .map_err(|err| format!("Could not read the SQL file: {err}"))?;
    let head = String::from_utf8_lossy(&head[..head.len().min(4096)]);
    Ok(head.lines().any(|line| {
        let line = line.trim_start_matches('\u{feff}').trim_start();
        DUMP_HEADERS.iter().any(|header| line.starts_with(header))
    }))
}

struct PlannedStmt {
    index: usize,
    line: usize,
    sql: String,
}

struct Phase1Plan {
    data: Vec<DataChunk>,
    data_statements: Vec<u64>,
    post: Vec<PlannedStmt>,
    total_bytes: u64,
    /// Extra connections, already set up, for phase 2.
    extras: Vec<Conn>,
}

fn is_lock_or_unlock_tables(sql: &str) -> bool {
    let mut words = Words::new(sql);
    let Some(first) = words.next() else {
        return false;
    };
    if !first.eq_ignore_ascii_case("LOCK") && !first.eq_ignore_ascii_case("UNLOCK") {
        return false;
    }
    words
        .next()
        .is_some_and(|second| second.eq_ignore_ascii_case("TABLES"))
}

/**
 * A MySQL `SET` of the server's GTID state. It runs once on the coordinator
 * like any other statement, but isn't repeated on the extra workers, where
 * setting `GTID_PURGED` a second time fails.
 */
fn is_mysql_gtid_set(sql: &str) -> bool {
    let mut words = Words::new(sql);
    if !words.next().is_some_and(|w| w.eq_ignore_ascii_case("SET")) {
        return false;
    }
    while let Some(word) = words.next() {
        let rest = &sql[words.index..];
        let trimmed = rest.trim_start();
        if !trimmed.starts_with('=') && !trimmed.starts_with(":=") {
            continue;
        }
        let name = word.rsplit('.').next().unwrap_or(word);
        if name.eq_ignore_ascii_case("GTID_NEXT") || name.eq_ignore_ascii_case("GTID_PURGED") {
            return true;
        }
    }
    false
}

/// A session statement phase 1 repeats on every extra worker, so they all run with the same settings.
fn replays_on_workers(driver: Driver, sql: &str) -> bool {
    let class = resume::classify(driver, sql);
    class.session && class.tx == Tx::None && !(driver == Driver::Mysql && is_mysql_gtid_set(sql))
}

fn is_tx_begin(sql: &str) -> bool {
    let mut words = Words::new(sql);
    let Some(first) = words.next() else {
        return false;
    };
    if first.eq_ignore_ascii_case("BEGIN") {
        return !words.next().is_some_and(|w| w.eq_ignore_ascii_case("NOT"));
    }
    first.eq_ignore_ascii_case("START")
        && words
            .next()
            .is_some_and(|w| w.eq_ignore_ascii_case("TRANSACTION"))
}

fn is_tx_end(sql: &str) -> bool {
    let mut words = Words::new(sql);
    let Some(first) = words.next() else {
        return false;
    };
    if first.eq_ignore_ascii_case("COMMIT") {
        return true;
    }
    if first.eq_ignore_ascii_case("ROLLBACK") {
        return !words.any(|w| w.eq_ignore_ascii_case("TO"));
    }
    first.eq_ignore_ascii_case("END")
        && match words.next() {
            None => true,
            Some(w) => w.eq_ignore_ascii_case("TRANSACTION") || w.eq_ignore_ascii_case("WORK"),
        }
}

enum PreMsg {
    Stmt {
        index: usize,
        line: usize,
        sql: String,
        text_bytes: u64,
        ack: Option<tokio::sync::oneshot::Sender<()>>,
    },
    Session {
        sql: String,
    },
    Fence {
        ack: tokio::sync::oneshot::Sender<()>,
    },
}

struct SpawnedPhase1Workers(Vec<tokio::task::JoinHandle<(Conn, bool)>>);

impl Drop for SpawnedPhase1Workers {
    fn drop(&mut self) {
        for handle in &self.0 {
            handle.abort();
        }
    }
}

fn fail_phase1(
    error: &Mutex<Option<(usize, LoadSqlError)>>,
    cancel: &AtomicBool,
    notify: &tokio::sync::Notify,
    index: usize,
    failure: impl Into<LoadSqlError>,
) {
    let failure = failure.into();
    let mut slot = lock(error);
    match &mut *slot {
        None => *slot = Some((index, failure)),
        Some((existing_idx, existing)) => {
            let both_lost = matches!(existing, LoadSqlError::LostConnection(_))
                && matches!(failure, LoadSqlError::LostConnection(_));
            if both_lost {
                existing.absorb(failure);
                *existing_idx = (*existing_idx).min(index);
            } else if index < *existing_idx {
                *slot = Some((index, failure));
            }
        }
    }
    cancel.store(true, Ordering::Relaxed);
    notify.notify_waiters();
}

fn stop_phase1(
    error: &Mutex<Option<(usize, LoadSqlError)>>,
    cancel: &AtomicBool,
    notify: &tokio::sync::Notify,
    index: usize,
    line: Option<usize>,
) -> bool {
    if lock(error).is_some() {
        return true;
    }
    if !cancel.load(Ordering::Relaxed) {
        return false;
    }
    let message = match line {
        Some(line) => format!("Line {line}: Import cancelled."),
        None => "Import cancelled.".to_string(),
    };
    fail_phase1(error, cancel, notify, index, message);
    true
}

fn dec_in_flight(in_flight: &AtomicU64, total_in_flight: &AtomicU64) {
    let mut cur = in_flight.load(Ordering::Relaxed);
    while cur > 0 {
        match in_flight.compare_exchange_weak(cur, cur - 1, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(actual) => cur = actual,
        }
    }
    let mut total = total_in_flight.load(Ordering::Relaxed);
    while total > 0 {
        match total_in_flight.compare_exchange_weak(
            total,
            total - 1,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(actual) => total = actual,
        }
    }
}

const DEADLOCK_RETRY_DELAYS_MS: [u64; 8] = [10, 25, 50, 100, 200, 400, 800, 1500];

fn is_retryable_deadlock(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("deadlock found when trying to get lock")
        || lower.contains("lock wait timeout exceeded; try restarting transaction")
        || lower.contains("deadlock detected")
}

async fn execute_sql_retry(conn: &mut Conn, sql: &str, cancel: &AtomicBool) -> Result<u64, String> {
    let mut attempt = 0usize;
    loop {
        match conn.execute(sql).await {
            Ok(rows) => return Ok(rows),
            Err(err) => {
                if attempt < DEADLOCK_RETRY_DELAYS_MS.len()
                    && !cancel.load(Ordering::Relaxed)
                    && !is_connection_lost(&err)
                    && is_retryable_deadlock(&err)
                {
                    let delay = DEADLOCK_RETRY_DELAYS_MS[attempt];
                    attempt += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                    continue;
                }
                return Err(err);
            }
        }
    }
}

async fn execute_batch_retry(
    conn: &mut Conn,
    batch_sql: &str,
    cancel: &AtomicBool,
) -> Result<u64, String> {
    let mut attempt = 0usize;
    loop {
        match conn.execute(batch_sql).await {
            Ok(rows) => return Ok(rows),
            Err(err) => {
                if attempt < DEADLOCK_RETRY_DELAYS_MS.len()
                    && !cancel.load(Ordering::Relaxed)
                    && !is_connection_lost(&err)
                    && is_retryable_deadlock(&err)
                {
                    let _ = conn.execute("ROLLBACK").await;
                    let delay = DEADLOCK_RETRY_DELAYS_MS[attempt];
                    attempt += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                    continue;
                }
                return Err(err);
            }
        }
    }
}

/**
 * Runs `stmts` in one transaction. When that fails for any reason but a
 * dropped connection, rolls it back and runs them one at a time, so a failure
 * names its own statement. With `wrap` off, as inside the file's own
 * transaction, runs them one at a time from the start. `done` gets each run
 * of statements that finished, in file order.
 */
async fn run_batch(
    conn: &mut Conn,
    stmts: &[SqlStmt],
    wrap: bool,
    batch_sql: &mut String,
    cancel: &AtomicBool,
    mut done: impl FnMut(&[SqlStmt]),
) -> Result<(), LoadSqlError> {
    if wrap && stmts.len() > 1 {
        build_batch_sql(batch_sql, stmts);
        match execute_batch_retry(conn, batch_sql, cancel).await {
            Ok(_) => {
                done(stmts);
                return Ok(());
            }
            Err(err) if is_connection_lost(&err) => {
                return Err(LoadSqlError::LostConnection(LoadLostConnection {
                    in_flight: stmts.iter().map(|stmt| stmt.index).collect(),
                    source: err,
                }));
            }
            Err(_) => {
                conn.execute("ROLLBACK").await.map_err(between_statements)?;
            }
        }
    }
    for (at, stmt) in stmts.iter().enumerate() {
        execute_sql_retry(conn, &stmt.sql, cancel)
            .await
            .map_err(|err| statement_failure(stmt.index, stmt.line, err))?;
        done(&stmts[at..=at]);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn exec_phase1_extra_worker(
    conn: &mut Conn,
    worker: usize,
    init_preamble: &[String],
    mut rx: tokio::sync::mpsc::UnboundedReceiver<PreMsg>,
    cancel: &AtomicBool,
    notify: &tokio::sync::Notify,
    error: &Mutex<Option<(usize, LoadSqlError)>>,
    in_flight: &AtomicU64,
    total_in_flight: &AtomicU64,
    track: Option<&LoadTrack>,
    progress_tx: &tokio::sync::mpsc::UnboundedSender<(u64, usize)>,
) -> bool {
    for sql in init_preamble {
        if stop_phase1(error, cancel, notify, usize::MAX, None) {
            in_flight.store(0, Ordering::Relaxed);
            return false;
        }
        if let Err(err) = execute_sql_retry(conn, sql, cancel).await {
            in_flight.store(0, Ordering::Relaxed);
            fail_phase1(error, cancel, notify, usize::MAX, worker_setup_failure(err));
            return false;
        }
    }
    while let Some(msg) = rx.recv().await {
        match msg {
            PreMsg::Stmt {
                index,
                line,
                sql,
                text_bytes,
                ack,
            } => {
                if stop_phase1(error, cancel, notify, index, Some(line)) {
                    dec_in_flight(in_flight, total_in_flight);
                    if let Some(ack) = ack {
                        let _ = ack.send(());
                    }
                    return false;
                }
                if let Err(err) = execute_sql_retry(conn, &sql, cancel).await {
                    dec_in_flight(in_flight, total_in_flight);
                    fail_phase1(
                        error,
                        cancel,
                        notify,
                        index,
                        statement_failure(index, line, err),
                    );
                    if let Some(ack) = ack {
                        let _ = ack.send(());
                    }
                    return false;
                }
                if let Some(track) = track {
                    track.bump_pre(worker);
                }
                let _ = progress_tx.send((text_bytes, index));
                dec_in_flight(in_flight, total_in_flight);
                if let Some(ack) = ack {
                    let _ = ack.send(());
                }
            }
            PreMsg::Session { sql } => {
                let res = if stop_phase1(error, cancel, notify, usize::MAX, None) {
                    Err(())
                } else if let Err(err) = execute_sql_retry(conn, &sql, cancel).await {
                    fail_phase1(error, cancel, notify, usize::MAX, worker_setup_failure(err));
                    Err(())
                } else {
                    Ok(())
                };
                dec_in_flight(in_flight, total_in_flight);
                if res.is_err() {
                    return false;
                }
            }
            PreMsg::Fence { ack } => {
                dec_in_flight(in_flight, total_in_flight);
                let _ = ack.send(());
            }
        }
    }
    true
}

#[allow(clippy::too_many_arguments)]
async fn run_phase1<R, Report>(
    coordinator: &mut Conn,
    reader: R,
    driver: Driver,
    workers: usize,
    split_tables: bool,
    extras: Vec<Conn>,
    ctx: &RunCtx<'_, Report>,
    temps: &mut TempFiles,
) -> Result<Phase1Plan, LoadSqlError>
where
    R: BufRead,
    Report: FnMut(LoadProgress) + Send,
{
    if workers <= 1 {
        close_conns(extras).await;
        return run_phase1_sequential(coordinator, reader, driver, ctx, temps).await;
    }
    run_phase1_parallel(
        coordinator,
        reader,
        driver,
        workers,
        split_tables,
        extras,
        ctx,
        temps,
    )
    .await
}

/// Counts what phase 1 read since it last yielded, so splitting a large file doesn't starve the workers.
#[derive(Default)]
struct Pause {
    statements: usize,
    bytes: u64,
}

impl Pause {
    async fn after(&mut self, bytes: u64, max_bytes: u64, max_statements: usize) {
        self.statements += 1;
        self.bytes += bytes;
        if self.bytes >= max_bytes || self.statements >= max_statements {
            self.reset();
            tokio::task::yield_now().await;
        }
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

async fn run_phase1_sequential<R, Report>(
    coordinator: &mut Conn,
    reader: R,
    driver: Driver,
    ctx: &RunCtx<'_, Report>,
    temps: &mut TempFiles,
) -> Result<Phase1Plan, LoadSqlError>
where
    R: BufRead,
    Report: FnMut(LoadProgress) + Send,
{
    let mut stream = Stream::new(reader, driver);
    let mut spool = Spool::new(1, false);
    let mut post: Vec<PlannedStmt> = Vec::new();
    let mut seen_data = false;
    let mut total_bytes = 0u64;
    // File order, including LOCK TABLES statements workers > 1 will not run.
    let mut next_index = 0usize;
    let mut pause = Pause::default();
    let track = lock(ctx.progress).track.clone();

    while let Some(stmt) = stream.next_statement(ctx.cancel)? {
        let index = next_index;
        next_index += 1;
        if ctx.cancel.load(Ordering::Relaxed) {
            return Err(line_error(stmt.line, "Import cancelled.").into());
        }
        if driver == Driver::Postgres && is_copy_from_stdin(&stmt.sql) {
            if let Some(track) = &track {
                track.plan_data();
            }
            let text_bytes = spool.push_copy(index, &stmt, &mut stream, ctx.cancel, temps)?;
            total_bytes += text_bytes;
            seen_data = true;
            stream.recycle(stmt.sql);
            pause
                .after(text_bytes, PHASE1_YIELD_BYTES, PHASE1_YIELD_STMTS)
                .await;
            continue;
        }
        let (decision, target_table) = decide_phase_with_table(&stmt.sql, seen_data, 1);
        match decision {
            Decision::Skip => {
                stream.recycle(stmt.sql);
            }
            Decision::Data => {
                if let Some(track) = &track {
                    track.plan_data();
                }
                let text_bytes = spool.push_sql(index, &stmt, target_table, temps)?;
                total_bytes += text_bytes;
                seen_data = true;
                stream.recycle(stmt.sql);
                pause
                    .after(text_bytes, PHASE1_YIELD_BYTES, PHASE1_YIELD_STMTS)
                    .await;
            }
            Decision::Pre => {
                let text_bytes = stmt.sql.len() as u64;
                if let Some(track) = &track {
                    track.plan_pre(0);
                }
                let tx_begin = is_tx_begin(&stmt.sql);
                let skip_exec = ctx.skip_completed.contains(&index)
                    || (driver == Driver::Mysql && is_lock_or_unlock_tables(&stmt.sql))
                    || (tx_begin && ctx.in_tx.load(Ordering::Relaxed));
                if !skip_exec {
                    if let Err(err) = execute_sql_retry(coordinator, &stmt.sql, ctx.cancel).await {
                        return Err(statement_failure(index, stmt.line, err));
                    }
                    if tx_begin {
                        ctx.in_tx.store(true, Ordering::Relaxed);
                    }
                }
                total_bytes += text_bytes;
                if let Some(track) = &track {
                    track.bump_pre(0);
                }
                note_pre_progress(ctx.progress, text_bytes, index);
                stream.recycle(stmt.sql);
                pause.reset();
            }
            Decision::Post => {
                let text_bytes = stmt.sql.len() as u64;
                post.push(PlannedStmt {
                    index,
                    line: stmt.line,
                    sql: stmt.sql,
                });
                total_bytes += text_bytes;
                pause
                    .after(text_bytes, PHASE1_YIELD_BYTES, PHASE1_YIELD_STMTS)
                    .await;
            }
        }
    }

    let (data, data_statements) = spool.finish()?;
    Ok(Phase1Plan {
        data,
        data_statements,
        post,
        total_bytes,
        extras: Vec::new(),
    })
}

#[allow(clippy::too_many_arguments)]
async fn run_phase1_parallel<R, Report>(
    coordinator: &mut Conn,
    reader: R,
    driver: Driver,
    workers: usize,
    split_tables: bool,
    extras: Vec<Conn>,
    ctx: &RunCtx<'_, Report>,
    temps: &mut TempFiles,
) -> Result<Phase1Plan, LoadSqlError>
where
    R: BufRead,
    Report: FnMut(LoadProgress) + Send,
{
    let track = lock(ctx.progress).track.clone();
    let connections = 1 + extras.len();
    let worker_cancel = Arc::new(AtomicBool::new(ctx.cancel.load(Ordering::Relaxed)));
    let cancel_notify = Arc::new(tokio::sync::Notify::new());
    let worker_error: Arc<Mutex<Option<(usize, LoadSqlError)>>> = Arc::new(Mutex::new(None));
    let in_flight_per_worker: Vec<Arc<AtomicU64>> = (0..connections)
        .map(|_| Arc::new(AtomicU64::new(0)))
        .collect();
    let total_in_flight = Arc::new(AtomicU64::new(0));
    let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel::<(u64, usize)>();

    let (tx0, rx0) = tokio::sync::mpsc::unbounded_channel();
    let mut txs = Vec::with_capacity(connections);
    txs.push(tx0);
    let mut handles = SpawnedPhase1Workers(Vec::with_capacity(extras.len()));
    let init: Arc<[String]> = Arc::from(ctx.preamble);
    for (offset, mut conn) in extras.into_iter().enumerate() {
        let worker = offset + 1;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        txs.push(tx);
        let init = init.clone();
        let cancel = worker_cancel.clone();
        let notify = cancel_notify.clone();
        let error = worker_error.clone();
        let in_flight = in_flight_per_worker[worker].clone();
        let total = total_in_flight.clone();
        let track = track.clone();
        let progress_tx = progress_tx.clone();
        handles.0.push(tokio::spawn(async move {
            let ok = exec_phase1_extra_worker(
                &mut conn,
                worker,
                &init,
                rx,
                &cancel,
                &notify,
                &error,
                &in_flight,
                &total,
                track.as_ref(),
                &progress_tx,
            )
            .await;
            (conn, ok)
        }));
    }
    drop(progress_tx);

    let sync_cancel = || {
        if ctx.cancel.load(Ordering::Relaxed) {
            worker_cancel.store(true, Ordering::Relaxed);
        } else if worker_cancel.load(Ordering::Relaxed) {
            ctx.cancel.store(true, Ordering::Relaxed);
        }
    };

    let reader_in_flight = in_flight_per_worker.clone();
    let reader_total_in_flight = total_in_flight.clone();
    let reader_error = worker_error.clone();
    let reader_cancel = worker_cancel.clone();
    let reader_track = track.clone();

    let reader_fut = async move {
        let mut stream = Stream::new(reader, driver);
        let mut spool = Spool::new(connections, split_tables);
        let mut post: Vec<PlannedStmt> = Vec::new();
        let mut seen_data = false;
        let mut total_bytes = 0u64;
        let mut next_index = 0usize;
        let mut pause = Pause::default();

        let mut ddl_table_workers: HashMap<String, usize> = HashMap::new();
        let mut fk_tables: HashSet<String> = HashSet::new();
        let mut worker_assigned_tables = vec![0usize; connections];
        let mut dirty_workers = vec![false; connections];

        while let Some(stmt) = stream.next_statement(&reader_cancel)? {
            let index = next_index;
            next_index += 1;
            if lock(&reader_error).is_some() {
                break;
            }
            if ctx.cancel.load(Ordering::Relaxed) || reader_cancel.load(Ordering::Relaxed) {
                return Err(line_error(stmt.line, "Import cancelled.").into());
            }
            let (max_bytes, max_statements) = if reader_total_in_flight.load(Ordering::Relaxed) > 0
            {
                (64 * 1024, 64)
            } else {
                (PHASE1_YIELD_BYTES, PHASE1_YIELD_STMTS)
            };
            if driver == Driver::Postgres && is_copy_from_stdin(&stmt.sql) {
                if let Some(track) = &reader_track {
                    track.plan_data();
                }
                let text_bytes =
                    spool.push_copy(index, &stmt, &mut stream, &reader_cancel, temps)?;
                total_bytes += text_bytes;
                seen_data = true;
                stream.recycle(stmt.sql);
                pause.after(text_bytes, max_bytes, max_statements).await;
                continue;
            }
            let (decision, target_table) = decide_phase_with_table(&stmt.sql, seen_data, workers);
            match decision {
                Decision::Skip => {
                    stream.recycle(stmt.sql);
                }
                Decision::Data => {
                    if let Some(track) = &reader_track {
                        track.plan_data();
                    }
                    let text_bytes = spool.push_sql(index, &stmt, target_table, temps)?;
                    total_bytes += text_bytes;
                    seen_data = true;
                    stream.recycle(stmt.sql);
                    pause.after(text_bytes, max_bytes, max_statements).await;
                }
                Decision::Pre => {
                    let text_bytes = stmt.sql.len() as u64;
                    let skip_exec = ctx.skip_completed.contains(&index)
                        || (driver == Driver::Mysql && is_lock_or_unlock_tables(&stmt.sql))
                        || is_tx_begin(&stmt.sql);
                    if skip_exec {
                        if let Some(track) = &reader_track {
                            track.plan_pre(0);
                            track.bump_pre(0);
                        }
                        total_bytes += text_bytes;
                        note_pre_progress(ctx.progress, text_bytes, index);
                        stream.recycle(stmt.sql);
                        continue;
                    }

                    collect_fk_tables(&stmt.sql, &mut fk_tables);
                    if let Some(table) = pre_target_table(driver, &stmt.sql)
                        .filter(|table| !fk_tables.contains(table.as_str()))
                    {
                        let w = if let Some(&existing) = ddl_table_workers.get(table.as_str()) {
                            existing
                        } else {
                            let chosen = (0..connections)
                                .min_by_key(|&w| {
                                    (
                                        reader_in_flight[w].load(Ordering::Relaxed),
                                        worker_assigned_tables[w],
                                        w,
                                    )
                                })
                                .unwrap_or(0);
                            ddl_table_workers.insert(table, chosen);
                            worker_assigned_tables[chosen] += 1;
                            chosen
                        };
                        dirty_workers[w] = true;
                        if let Some(track) = &reader_track {
                            track.plan_pre(w);
                        }
                        let cur_in_flight = reader_in_flight[w].fetch_add(1, Ordering::Relaxed) + 1;
                        reader_total_in_flight.fetch_add(1, Ordering::Relaxed);
                        let (ack, backpressure) = if cur_in_flight >= 64 {
                            let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
                            (Some(ack_tx), Some(ack_rx))
                        } else {
                            (None, None)
                        };
                        let _ = txs[w].send(PreMsg::Stmt {
                            index,
                            line: stmt.line,
                            sql: stmt.sql,
                            text_bytes,
                            ack,
                        });
                        total_bytes += text_bytes;
                        pause.reset();
                        if let Some(ack_rx) = backpressure {
                            let _ = ack_rx.await;
                        } else {
                            tokio::task::yield_now().await;
                        }
                        if lock(&reader_error).is_some() {
                            break;
                        }
                        continue;
                    }

                    // Barrier statement: wait for any in-flight table DDL across workers first.
                    let mut fences = Vec::new();
                    for (w, dirty) in dirty_workers.iter_mut().enumerate() {
                        if *dirty {
                            *dirty = false;
                            reader_in_flight[w].fetch_add(1, Ordering::Relaxed);
                            reader_total_in_flight.fetch_add(1, Ordering::Relaxed);
                            let (fence_tx, fence_rx) = tokio::sync::oneshot::channel();
                            if txs[w].send(PreMsg::Fence { ack: fence_tx }).is_ok() {
                                fences.push(fence_rx);
                            } else {
                                dec_in_flight(&reader_in_flight[w], &reader_total_in_flight);
                            }
                        }
                    }
                    for fence_rx in fences {
                        let _ = fence_rx.await;
                    }
                    if lock(&reader_error).is_some() {
                        break;
                    }

                    if let Some(track) = &reader_track {
                        track.plan_pre(0);
                    }
                    let session_copy =
                        replays_on_workers(driver, &stmt.sql).then(|| stmt.sql.clone());

                    reader_in_flight[0].fetch_add(1, Ordering::Relaxed);
                    reader_total_in_flight.fetch_add(1, Ordering::Relaxed);
                    let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
                    let _ = txs[0].send(PreMsg::Stmt {
                        index,
                        line: stmt.line,
                        sql: stmt.sql,
                        text_bytes,
                        ack: Some(ack_tx),
                    });
                    let _ = ack_rx.await;
                    if lock(&reader_error).is_some() {
                        break;
                    }
                    if let Some(session_sql) = session_copy {
                        for (w, tx) in txs.iter().enumerate().skip(1) {
                            reader_in_flight[w].fetch_add(1, Ordering::Relaxed);
                            reader_total_in_flight.fetch_add(1, Ordering::Relaxed);
                            if tx
                                .send(PreMsg::Session {
                                    sql: session_sql.clone(),
                                })
                                .is_err()
                            {
                                dec_in_flight(&reader_in_flight[w], &reader_total_in_flight);
                            }
                        }
                    }
                    total_bytes += text_bytes;
                    pause.reset();
                }
                Decision::Post => {
                    let text_bytes = stmt.sql.len() as u64;
                    post.push(PlannedStmt {
                        index,
                        line: stmt.line,
                        sql: stmt.sql,
                    });
                    total_bytes += text_bytes;
                    pause.after(text_bytes, max_bytes, PHASE1_YIELD_STMTS).await;
                }
            }
        }

        drop(txs);
        Ok::<_, LoadSqlError>((spool, post, total_bytes))
    };

    let coord_in_flight = in_flight_per_worker[0].clone();
    let coord_total_in_flight = total_in_flight.clone();
    let coord_error = worker_error.clone();
    let coord_cancel = worker_cancel.clone();
    let coord_notify = cancel_notify.clone();
    let coord_track = track.clone();

    let coord_worker = async move {
        let mut rx0 = rx0;
        while let Some(msg) = rx0.recv().await {
            sync_cancel();
            match msg {
                PreMsg::Stmt {
                    index,
                    line,
                    sql,
                    text_bytes,
                    ack,
                } => {
                    if stop_phase1(
                        &coord_error,
                        &coord_cancel,
                        &coord_notify,
                        index,
                        Some(line),
                    ) {
                        dec_in_flight(&coord_in_flight, &coord_total_in_flight);
                        if let Some(ack) = ack {
                            let _ = ack.send(());
                        }
                        break;
                    }
                    if let Err(err) = execute_sql_retry(coordinator, &sql, &coord_cancel).await {
                        dec_in_flight(&coord_in_flight, &coord_total_in_flight);
                        fail_phase1(
                            &coord_error,
                            &coord_cancel,
                            &coord_notify,
                            index,
                            statement_failure(index, line, err),
                        );
                        if let Some(ack) = ack {
                            let _ = ack.send(());
                        }
                        break;
                    }
                    if let Some(track) = &coord_track {
                        track.bump_pre(0);
                    }
                    note_pre_progress(ctx.progress, text_bytes, index);
                    dec_in_flight(&coord_in_flight, &coord_total_in_flight);
                    if let Some(ack) = ack {
                        let _ = ack.send(());
                    }
                }
                PreMsg::Session { .. } => {
                    dec_in_flight(&coord_in_flight, &coord_total_in_flight);
                }
                PreMsg::Fence { ack } => {
                    dec_in_flight(&coord_in_flight, &coord_total_in_flight);
                    let _ = ack.send(());
                }
            }
        }
    };

    let progress_drainer = async {
        loop {
            sync_cancel();
            tokio::select! {
                biased;
                event = progress_rx.recv() => {
                    match event {
                        Some((text_bytes, index)) => {
                            note_pre_progress(ctx.progress, text_bytes, index);
                        }
                        None => break,
                    }
                }
                () = cancel_notify.notified() => {
                    sync_cancel();
                }
            }
        }
    };

    let ((reader_res, ()), ()) = futures_util::future::join(
        futures_util::future::join(reader_fut, coord_worker),
        progress_drainer,
    )
    .await;
    let mut extras = Vec::with_capacity(handles.0.len());
    let mut broken = Vec::new();
    for handle in std::mem::take(&mut handles.0) {
        if let Ok((conn, ok)) = handle.await {
            if ok {
                extras.push(conn);
            } else {
                broken.push(conn);
            }
        }
    }
    close_conns(broken).await;
    sync_cancel();
    let taken_err = lock(&worker_error).take();
    if let Some((_, err)) = taken_err {
        close_conns(extras).await;
        return Err(err);
    }
    let (spool, post, total_bytes) = match reader_res {
        Ok(parts) => parts,
        Err(err) => {
            close_conns(extras).await;
            return Err(err);
        }
    };
    if ctx.cancel.load(Ordering::Relaxed) {
        close_conns(extras).await;
        return Err("Import cancelled.".to_string().into());
    }
    let (data, data_statements) = match spool.finish() {
        Ok(res) => res,
        Err(err) => {
            close_conns(extras).await;
            return Err(err.into());
        }
    };
    Ok(Phase1Plan {
        data,
        data_statements,
        post,
        total_bytes,
        extras,
    })
}

async fn run_phase2<Report>(
    coordinator: &mut Conn,
    chunks: &[DataChunk],
    mut extras: Vec<Conn>,
    setup: &[String],
    split_tables: bool,
    ctx: &RunCtx<'_, Report>,
) -> Result<(), LoadSqlError>
where
    Report: FnMut(LoadProgress) + Send,
{
    if chunks.is_empty() {
        close_conns(extras).await;
        return Ok(());
    }
    let groups = assign_workers(chunks, 1 + extras.len());
    let extra_count = groups.len().saturating_sub(1);
    if extras.len() > extra_count {
        let unused = extras.split_off(extra_count);
        close_conns(unused).await;
    }
    if stop_requested(ctx.error, ctx.cancel, None) {
        close_conns(extras).await;
        return take_error(ctx.error, ctx.cancel);
    }

    if extras.is_empty() {
        exec_group(coordinator, &groups[0], 0, ctx).await;
        return take_error(ctx.error, ctx.cancel);
    }

    let worker_cancel = Arc::new(AtomicBool::new(ctx.cancel.load(Ordering::Relaxed)));
    let cancel_notify = Arc::new(tokio::sync::Notify::new());
    let worker_error = Arc::new(Mutex::new(lock(ctx.error).clone()));
    let setup: Arc<[String]> = Arc::from(setup);
    let skip_completed = Arc::new(ctx.skip_completed.clone());
    let track = lock(ctx.progress).track.clone();
    let shared_queues = match SharedQueues::new(&groups, split_tables) {
        Ok(q) => Arc::new(q),
        Err(err) => {
            fail(ctx.error, ctx.cancel, err);
            close_conns(extras).await;
            return take_error(ctx.error, ctx.cancel);
        }
    };
    let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel();

    let mut handles = SpawnedWorkers(Vec::with_capacity(extras.len()));
    for (offset, mut conn) in extras.into_iter().enumerate() {
        let task_ctx = Phase2Ctx {
            setup: setup.clone(),
            cancel: worker_cancel.clone(),
            cancel_notify: cancel_notify.clone(),
            error: worker_error.clone(),
            skip_completed: skip_completed.clone(),
            track: track.clone(),
            queues: shared_queues.clone(),
            progress_tx: progress_tx.clone(),
        };
        handles.0.push(tokio::spawn(async move {
            exec_phase2_worker(&mut conn, offset + 1, true, &task_ctx).await;
            conn
        }));
    }

    let coord_ctx = Phase2Ctx {
        setup,
        cancel: worker_cancel.clone(),
        cancel_notify: cancel_notify.clone(),
        error: worker_error.clone(),
        skip_completed,
        track,
        queues: shared_queues,
        progress_tx,
    };
    let run_workers = async {
        exec_phase2_worker(coordinator, 0, false, &coord_ctx).await;
        drop(coord_ctx);
        let mut finished = Vec::with_capacity(handles.0.len());
        for handle in std::mem::take(&mut handles.0) {
            if let Ok(conn) = handle.await {
                finished.push(conn);
            }
        }
        finished
    };
    let sync_cancel = || {
        if ctx.cancel.load(Ordering::Relaxed) {
            worker_cancel.store(true, Ordering::Relaxed);
        } else if worker_cancel.load(Ordering::Relaxed) {
            ctx.cancel.store(true, Ordering::Relaxed);
        }
    };
    let drain_progress = async {
        loop {
            sync_cancel();
            tokio::select! {
                biased;
                event = progress_rx.recv() => {
                    match event {
                        Some(batch) => {
                            note_progress_batch(ctx.progress, batch);
                        }
                        None => break,
                    }
                }
                () = cancel_notify.notified() => {
                    sync_cancel();
                }
            }
        }
    };
    let (returned_extras, ()) = futures_util::future::join(run_workers, drain_progress).await;
    sync_cancel();
    if let Some(err) = lock(&worker_error).take() {
        fail(ctx.error, ctx.cancel, err);
    }
    close_conns(returned_extras).await;
    take_error(ctx.error, ctx.cancel)
}

struct SpawnedWorkers(Vec<tokio::task::JoinHandle<Conn>>);

impl Drop for SpawnedWorkers {
    fn drop(&mut self) {
        for handle in &self.0 {
            handle.abort();
        }
    }
}

struct SqlStmt {
    index: usize,
    line: usize,
    sql: String,
}

enum PoppedBatch {
    Sql(Vec<SqlStmt>),
    /// A `COPY` header, with the chunk it came from, whose reader is at the rows. Hand the chunk back with [`SharedQueues::put_back`].
    Copy {
        index: usize,
        line: usize,
        sql: String,
        chunk: WorkerDeqs,
    },
}

/// Statements one worker finished, with their sizes.
struct ProgressBatch {
    worker: usize,
    indexes: Vec<(usize, u64)>,
}

impl ProgressBatch {
    fn of(worker: usize, stmts: &[SqlStmt]) -> Self {
        Self {
            worker,
            indexes: stmts
                .iter()
                .map(|stmt| (stmt.index, stmt.sql.len() as u64))
                .collect(),
        }
    }
}

enum WorkerDeqs {
    Memory {
        table_id: usize,
        cursor: std::io::Cursor<Arc<[u8]>>,
        victim: usize,
        peeked: Option<NextRecord>,
    },
    Disk {
        table_id: usize,
        path: PathBuf,
        reader: Option<BufReader<File>>,
        victim: usize,
        peeked: Option<NextRecord>,
    },
}

enum NextRecord {
    Sql {
        index: usize,
        line: usize,
        sql: String,
    },
    /// Only the header. The rows follow in the chunk's reader.
    Copy {
        index: usize,
        line: usize,
        sql: String,
    },
}

impl WorkerDeqs {
    fn open(chunk: DataChunk, victim: usize) -> Result<Self, String> {
        let table_id = chunk.table_id;
        match chunk.source {
            ChunkSource::Memory(bytes) => {
                let mut cursor = std::io::Cursor::new(bytes);
                read_magic(&mut cursor)?;
                Ok(WorkerDeqs::Memory {
                    table_id,
                    cursor,
                    victim,
                    peeked: None,
                })
            }
            ChunkSource::Disk(path) => Ok(WorkerDeqs::Disk {
                table_id,
                path,
                reader: None,
                victim,
                peeked: None,
            }),
        }
    }

    fn table_id(&self) -> usize {
        match self {
            WorkerDeqs::Memory { table_id, .. } | WorkerDeqs::Disk { table_id, .. } => *table_id,
        }
    }

    fn victim(&self) -> usize {
        match self {
            WorkerDeqs::Memory { victim, .. } | WorkerDeqs::Disk { victim, .. } => *victim,
        }
    }

    /// Whether nothing has been read from this chunk yet.
    fn untouched(&self) -> bool {
        match self {
            WorkerDeqs::Memory { cursor, peeked, .. } => {
                peeked.is_none() && cursor.position() == MAGIC.len() as u64
            }
            WorkerDeqs::Disk { reader, .. } => reader.is_none(),
        }
    }

    fn push_back_record(&mut self, record: NextRecord) {
        match self {
            WorkerDeqs::Memory { peeked, .. } | WorkerDeqs::Disk { peeked, .. } => {
                *peeked = Some(record);
            }
        }
    }

    fn next_record(&mut self) -> Result<Option<NextRecord>, String> {
        match self {
            WorkerDeqs::Memory { cursor, peeked, .. } => {
                if let Some(rec) = peeked.take() {
                    return Ok(Some(rec));
                }
                read_next_record(cursor)
            }
            WorkerDeqs::Disk {
                path,
                reader,
                peeked,
                ..
            } => {
                if let Some(rec) = peeked.take() {
                    return Ok(Some(rec));
                }
                if reader.is_none() {
                    let file = File::open(&*path)
                        .map_err(|err| format!("Could not read the import chunk: {err}"))?;
                    let mut buf_reader = BufReader::with_capacity(CHUNK_IO_BUFFER, file);
                    read_magic(&mut buf_reader)?;
                    *reader = Some(buf_reader);
                }
                read_next_record(reader.as_mut().expect("disk chunk reader"))
            }
        }
    }

    /// The chunk's reader, at the rows of the `COPY` whose header was read last.
    fn payload(&mut self) -> Result<&mut (dyn Read + Send), String> {
        match self {
            WorkerDeqs::Memory { cursor, .. } => Ok(cursor),
            WorkerDeqs::Disk {
                reader: Some(reader),
                ..
            } => Ok(reader),
            WorkerDeqs::Disk { reader: None, .. } => {
                Err("The import chunk ended inside COPY data.".into())
            }
        }
    }
}

fn read_next_record<R: Read>(reader: &mut R) -> Result<Option<NextRecord>, String> {
    let Some(tag) = read_tag(reader)? else {
        return Ok(None);
    };
    match tag {
        TAG_SQL => {
            let index = read_usize(reader)?;
            let line = read_usize(reader)?;
            let sql = read_sql(reader)?;
            Ok(Some(NextRecord::Sql { index, line, sql }))
        }
        TAG_COPY => {
            let index = read_usize(reader)?;
            let line = read_usize(reader)?;
            let sql = read_sql(reader)?;
            Ok(Some(NextRecord::Copy { index, line, sql }))
        }
        _ => Err("The import chunk has an unknown record.".into()),
    }
}

struct SharedQueues {
    queues: Vec<Mutex<VecDeque<WorkerDeqs>>>,
    /// Whether an idle worker may take rows from a chunk another worker has started.
    split_tables: bool,
}

impl SharedQueues {
    fn new(groups: &[Vec<DataChunk>], split_tables: bool) -> Result<Self, String> {
        let mut queues = Vec::with_capacity(groups.len());
        for (worker, group) in groups.iter().enumerate() {
            let mut deq = VecDeque::with_capacity(group.len());
            for chunk in group {
                deq.push_back(WorkerDeqs::open(chunk.clone(), worker)?);
            }
            queues.push(Mutex::new(deq));
        }
        Ok(Self {
            queues,
            split_tables,
        })
    }

    /// Returns a chunk taken by [`PoppedBatch::Copy`], so `worker` carries on with its next record.
    fn put_back(&self, worker: usize, chunk: WorkerDeqs) {
        if let Some(slot) = self.queues.get(worker) {
            lock(slot).push_front(chunk);
        }
    }

    fn pop_batch_from_deq(
        deq: &mut VecDeque<WorkerDeqs>,
        thief: usize,
        track: Option<&LoadTrack>,
    ) -> Result<Option<PoppedBatch>, String> {
        let mut batch = Vec::new();
        let mut batch_bytes = 0usize;
        while let Some(front) = deq.front_mut() {
            let victim = front.victim();
            match front.next_record()? {
                Some(NextRecord::Sql { index, line, sql }) => {
                    if let Some(track) = track {
                        track.steal_one(thief, victim);
                    }
                    batch_bytes += sql.len();
                    batch.push(SqlStmt { index, line, sql });
                    if batch.len() >= BATCH_MAX_STMTS || batch_bytes >= BATCH_TARGET_BYTES {
                        return Ok(Some(PoppedBatch::Sql(batch)));
                    }
                }
                Some(NextRecord::Copy { index, line, sql }) => {
                    if !batch.is_empty() {
                        front.push_back_record(NextRecord::Copy { index, line, sql });
                        return Ok(Some(PoppedBatch::Sql(batch)));
                    }
                    if let Some(track) = track {
                        track.steal_one(thief, victim);
                    }
                    let chunk = deq.pop_front().expect("front chunk");
                    return Ok(Some(PoppedBatch::Copy {
                        index,
                        line,
                        sql,
                        chunk,
                    }));
                }
                None => {
                    deq.pop_front();
                    if !batch.is_empty() {
                        return Ok(Some(PoppedBatch::Sql(batch)));
                    }
                }
            }
        }
        Ok((!batch.is_empty()).then_some(PoppedBatch::Sql(batch)))
    }

    fn pop_batch(
        &self,
        worker: usize,
        track: Option<&LoadTrack>,
    ) -> Result<Option<PoppedBatch>, String> {
        if let Some(slot) = self.queues.get(worker) {
            let mut deq = lock(slot);
            if let Some(batch) = Self::pop_batch_from_deq(&mut deq, worker, track)? {
                return Ok(Some(batch));
            }
        }
        /*
         * Own queue is empty: take an untouched chunk from the back of another
         * worker's queue, preferring a different table than its front chunk,
         * so two workers do not contend on the same table while queued tables remain.
         */
        let n = self.queues.len();
        for require_different_table in [true, false] {
            for offset in 1..n {
                let other = (worker + offset) % n;
                let stolen = {
                    let mut deq = lock(&self.queues[other]);
                    let untouched = deq.back().is_some_and(WorkerDeqs::untouched);
                    let same_table = deq.len() > 1
                        && deq.front().map(WorkerDeqs::table_id)
                            == deq.back().map(WorkerDeqs::table_id);
                    if untouched && !(require_different_table && same_table) {
                        deq.pop_back()
                    } else {
                        None
                    }
                };
                if let Some(chunk_deq) = stolen {
                    if let Some(slot) = self.queues.get(worker) {
                        let mut own_deq = lock(slot);
                        own_deq.push_back(chunk_deq);
                        if let Some(batch) = Self::pop_batch_from_deq(&mut own_deq, worker, track)?
                        {
                            return Ok(Some(batch));
                        }
                    }
                }
            }
        }
        if !self.split_tables {
            return Ok(None);
        }
        // All remaining queues are down to chunks already started: steal a batch from the front.
        for offset in 1..n {
            let other = (worker + offset) % n;
            let mut deq = lock(&self.queues[other]);
            if let Some(batch) = Self::pop_batch_from_deq(&mut deq, worker, track)? {
                return Ok(Some(batch));
            }
        }
        Ok(None)
    }
}

struct Phase2Ctx {
    /// Run on each extra worker before its first batch.
    setup: Arc<[String]>,
    cancel: Arc<AtomicBool>,
    cancel_notify: Arc<tokio::sync::Notify>,
    error: Arc<Mutex<Option<LoadSqlError>>>,
    skip_completed: Arc<HashSet<usize>>,
    track: Option<LoadTrack>,
    queues: Arc<SharedQueues>,
    progress_tx: tokio::sync::mpsc::UnboundedSender<ProgressBatch>,
}

fn build_batch_sql(buf: &mut String, stmts: &[SqlStmt]) {
    buf.clear();
    let total_len: usize = stmts.iter().map(|s| s.sql.len() + 2).sum::<usize>() + 16;
    if buf.capacity() < total_len {
        buf.reserve(total_len - buf.capacity());
    }
    buf.push_str("BEGIN;\n");
    for stmt in stmts {
        buf.push_str(&stmt.sql);
        buf.push_str(";\n");
    }
    buf.push_str("COMMIT;");
}

async fn exec_phase2_worker(conn: &mut Conn, worker: usize, run_setup: bool, ctx: &Phase2Ctx) {
    if stop_requested(&ctx.error, &ctx.cancel, None) {
        ctx.cancel_notify.notify_waiters();
        return;
    }
    if let Some(track) = &ctx.track {
        track.mark_running(worker);
    }
    if run_setup {
        for sql in ctx.setup.iter() {
            if let Err(err) = conn.execute(sql).await {
                fail(&ctx.error, &ctx.cancel, worker_setup_failure(err));
                ctx.cancel_notify.notify_waiters();
                return;
            }
        }
    }
    let mut batch_sql = String::new();
    loop {
        if stop_requested(&ctx.error, &ctx.cancel, None) {
            ctx.cancel_notify.notify_waiters();
            return;
        }
        let popped = match ctx.queues.pop_batch(worker, ctx.track.as_ref()) {
            Ok(Some(batch)) => batch,
            Ok(None) => {
                if let Some(track) = &ctx.track {
                    track.finish_worker(worker);
                }
                return;
            }
            Err(err) => {
                fail(&ctx.error, &ctx.cancel, err);
                ctx.cancel_notify.notify_waiters();
                return;
            }
        };
        let result = match popped {
            PoppedBatch::Sql(stmts) => {
                let first_line = stmts.first().map(|s| s.line);
                if stop_requested(&ctx.error, &ctx.cancel, first_line) {
                    ctx.cancel_notify.notify_waiters();
                    return;
                }
                let (done, to_run): (Vec<SqlStmt>, Vec<SqlStmt>) = stmts
                    .into_iter()
                    .partition(|stmt| ctx.skip_completed.contains(&stmt.index));
                if !done.is_empty() {
                    let _ = ctx.progress_tx.send(ProgressBatch::of(worker, &done));
                }
                run_batch(conn, &to_run, true, &mut batch_sql, &ctx.cancel, |stmts| {
                    let _ = ctx.progress_tx.send(ProgressBatch::of(worker, stmts));
                })
                .await
            }
            PoppedBatch::Copy {
                index,
                line,
                sql,
                mut chunk,
            } => {
                if stop_requested(&ctx.error, &ctx.cancel, Some(line)) {
                    ctx.cancel_notify.notify_waiters();
                    return;
                }
                let copied = match chunk.payload() {
                    Ok(mut payload) => {
                        if ctx.skip_completed.contains(&index) {
                            drain_copy(&mut payload).map_err(|err| line_error(line, err).into())
                        } else {
                            execute_copy(conn, index, line, &sql, &mut payload, &ctx.cancel).await
                        }
                    }
                    Err(err) => Err(line_error(line, err).into()),
                };
                copied.map(|bytes| {
                    let _ = ctx.progress_tx.send(ProgressBatch {
                        worker,
                        indexes: vec![(index, bytes)],
                    });
                    ctx.queues.put_back(worker, chunk);
                })
            }
        };
        if let Err(err) = result {
            fail(&ctx.error, &ctx.cancel, err);
            ctx.cancel_notify.notify_waiters();
            return;
        }
    }
}

async fn exec_group<Report>(
    conn: &mut Conn,
    chunks: &[DataChunk],
    worker: usize,
    ctx: &RunCtx<'_, Report>,
) where
    Report: FnMut(LoadProgress) + Send,
{
    if stop_requested(ctx.error, ctx.cancel, None) {
        return;
    }
    let track = lock(ctx.progress).track.clone();
    if let Some(track) = track {
        track.mark_running(worker);
    }
    for chunk in chunks {
        if exec_chunk(conn, ctx, chunk, worker).await.is_err() {
            return;
        }
    }
    if let Some(track) = lock(ctx.progress).track.clone() {
        track.finish_worker(worker);
    }
}

async fn exec_post<Report>(
    conn: &mut Conn,
    driver: Driver,
    workers: usize,
    post: &[PlannedStmt],
    ctx: &RunCtx<'_, Report>,
) where
    Report: FnMut(LoadProgress) + Send,
{
    if stop_requested(ctx.error, ctx.cancel, None) {
        return;
    }
    let last_tx_end = post.iter().rposition(|stmt| is_tx_end(&stmt.sql));
    for (pos, stmt) in post.iter().enumerate() {
        if stop_requested(ctx.error, ctx.cancel, Some(stmt.line)) {
            return;
        }
        let tx_end = is_tx_end(&stmt.sql);
        let skip_exec = ctx.skip_completed.contains(&stmt.index)
            || (driver == Driver::Mysql && is_lock_or_unlock_tables(&stmt.sql))
            || (tx_end
                && (workers > 1 || !ctx.in_tx.load(Ordering::Relaxed) || Some(pos) != last_tx_end));
        if !skip_exec {
            if let Err(err) = execute_sql_retry(conn, &stmt.sql, ctx.cancel).await {
                fail(
                    ctx.error,
                    ctx.cancel,
                    statement_failure(stmt.index, stmt.line, err),
                );
                return;
            }
            if tx_end {
                ctx.in_tx.store(false, Ordering::Relaxed);
            }
        }
        note_post_progress(ctx.progress, stmt.sql.len() as u64, stmt.index);
    }
}

async fn exec_chunk<Report>(
    conn: &mut Conn,
    ctx: &RunCtx<'_, Report>,
    chunk: &DataChunk,
    worker: usize,
) -> Result<(), ()>
where
    Report: FnMut(LoadProgress) + Send,
{
    match &chunk.source {
        ChunkSource::Memory(bytes) => {
            let mut reader = std::io::Cursor::new(bytes.as_ref());
            exec_reader(conn, ctx, &mut reader, worker).await
        }
        ChunkSource::Disk(path) => {
            let file = File::open(path).map_err(|err| {
                fail(
                    ctx.error,
                    ctx.cancel,
                    format!("Could not read the import chunk: {err}"),
                );
            })?;
            let mut reader = BufReader::with_capacity(CHUNK_IO_BUFFER, file);
            exec_reader(conn, ctx, &mut reader, worker).await
        }
    }
}

async fn flush_reader_sql_batch<Report>(
    conn: &mut Conn,
    ctx: &RunCtx<'_, Report>,
    worker: usize,
    batch: &mut Vec<SqlStmt>,
    batch_bytes: &mut usize,
    batch_sql: &mut String,
) -> Result<(), ()>
where
    Report: FnMut(LoadProgress) + Send,
{
    if batch.is_empty() {
        return Ok(());
    }
    let wrap = !ctx.in_tx.load(Ordering::Relaxed);
    let result = run_batch(conn, batch, wrap, batch_sql, ctx.cancel, |stmts| {
        note_progress_batch(ctx.progress, ProgressBatch::of(worker, stmts));
    })
    .await;
    batch.clear();
    *batch_bytes = 0;
    result.map_err(|err| fail(ctx.error, ctx.cancel, err))
}

async fn exec_reader<Report, R>(
    conn: &mut Conn,
    ctx: &RunCtx<'_, Report>,
    reader: &mut R,
    worker: usize,
) -> Result<(), ()>
where
    Report: FnMut(LoadProgress) + Send,
    R: Read,
{
    read_magic(reader).map_err(|err| fail(ctx.error, ctx.cancel, err))?;
    let mut batch: Vec<SqlStmt> = Vec::with_capacity(BATCH_MAX_STMTS);
    let mut batch_bytes = 0usize;
    let mut batch_sql = String::new();
    loop {
        let tag = match read_tag(reader) {
            Ok(tag) => tag,
            Err(err) => {
                if !batch.is_empty() {
                    let _ = flush_reader_sql_batch(
                        conn,
                        ctx,
                        worker,
                        &mut batch,
                        &mut batch_bytes,
                        &mut batch_sql,
                    )
                    .await;
                }
                fail(ctx.error, ctx.cancel, err);
                return Err(());
            }
        };
        let Some(tag) = tag else {
            flush_reader_sql_batch(
                conn,
                ctx,
                worker,
                &mut batch,
                &mut batch_bytes,
                &mut batch_sql,
            )
            .await?;
            return Ok(());
        };
        match tag {
            TAG_SQL => {
                let index = read_usize(reader).map_err(|err| fail(ctx.error, ctx.cancel, err))?;
                let line = read_usize(reader).map_err(|err| fail(ctx.error, ctx.cancel, err))?;
                let sql = read_sql(reader).map_err(|err| fail(ctx.error, ctx.cancel, err))?;
                if stop_requested(ctx.error, ctx.cancel, Some(line)) {
                    return Err(());
                }
                if ctx.skip_completed.contains(&index) {
                    flush_reader_sql_batch(
                        conn,
                        ctx,
                        worker,
                        &mut batch,
                        &mut batch_bytes,
                        &mut batch_sql,
                    )
                    .await?;
                    note_progress(ctx.progress, sql.len() as u64, index, worker);
                    continue;
                }
                batch_bytes += sql.len();
                batch.push(SqlStmt { index, line, sql });
                if batch.len() >= BATCH_MAX_STMTS || batch_bytes >= BATCH_TARGET_BYTES {
                    flush_reader_sql_batch(
                        conn,
                        ctx,
                        worker,
                        &mut batch,
                        &mut batch_bytes,
                        &mut batch_sql,
                    )
                    .await?;
                }
            }
            TAG_COPY => {
                flush_reader_sql_batch(
                    conn,
                    ctx,
                    worker,
                    &mut batch,
                    &mut batch_bytes,
                    &mut batch_sql,
                )
                .await?;
                let index = read_usize(reader).map_err(|err| fail(ctx.error, ctx.cancel, err))?;
                let line = read_usize(reader).map_err(|err| fail(ctx.error, ctx.cancel, err))?;
                let sql = read_sql(reader).map_err(|err| fail(ctx.error, ctx.cancel, err))?;
                if stop_requested(ctx.error, ctx.cancel, Some(line)) {
                    return Err(());
                }
                if ctx.skip_completed.contains(&index) {
                    match drain_copy(reader) {
                        Ok(text_bytes) => note_progress(ctx.progress, text_bytes, index, worker),
                        Err(err) => {
                            fail(ctx.error, ctx.cancel, line_error(line, err));
                            return Err(());
                        }
                    }
                    continue;
                }
                match execute_copy(conn, index, line, &sql, reader, ctx.cancel).await {
                    Ok(text_bytes) => note_progress(ctx.progress, text_bytes, index, worker),
                    Err(err) => {
                        fail(ctx.error, ctx.cancel, err);
                        return Err(());
                    }
                }
            }
            _ => {
                let _ = flush_reader_sql_batch(
                    conn,
                    ctx,
                    worker,
                    &mut batch,
                    &mut batch_bytes,
                    &mut batch_sql,
                )
                .await;
                fail(
                    ctx.error,
                    ctx.cancel,
                    "The import chunk has an unknown record.".to_string(),
                );
                return Err(());
            }
        }
    }
}

/// Reads a COPY payload and discards it. Used when the COPY statement's index
/// is already complete, so the following record stays aligned.
fn drain_copy(reader: &mut impl Read) -> Result<u64, String> {
    loop {
        let tag = match read_tag(reader)? {
            Some(tag) => tag,
            None => return Err("The import chunk ended inside COPY data.".into()),
        };
        match tag {
            TAG_DATA => {
                read_vec(reader)?;
            }
            TAG_END => return read_u64(reader),
            _ => return Err("The import chunk has a broken COPY payload.".into()),
        }
    }
}

async fn execute_copy(
    conn: &mut Conn,
    index: usize,
    line: usize,
    sql: &str,
    reader: &mut impl Read,
    cancel: &AtomicBool,
) -> Result<u64, LoadSqlError> {
    let Conn::Postgres(pg) = conn else {
        return Err(statement_failure(
            index,
            line,
            "COPY FROM stdin only works with Postgres.".into(),
        ));
    };
    let mut copy = match pg.copy_in_raw(sql).await {
        Ok(copy) => copy,
        Err(err) => return Err(statement_failure(index, line, describe_error(err))),
    };
    let mut batch = Vec::new();
    let outcome = loop {
        if cancel.load(Ordering::Relaxed) {
            break Err("Import cancelled.".into());
        }
        let tag = match read_tag(reader) {
            Ok(Some(tag)) => tag,
            Ok(None) => break Err("The import chunk ended inside COPY data.".into()),
            Err(err) => break Err(err),
        };
        match tag {
            TAG_DATA => {
                let data = match read_vec(reader) {
                    Ok(data) => data,
                    Err(err) => break Err(err),
                };
                batch.extend_from_slice(&data);
                if batch.len() >= COPY_SEND {
                    if cancel.load(Ordering::Relaxed) {
                        break Err("Import cancelled.".into());
                    }
                    if let Err(err) = copy.send(batch.as_slice()).await {
                        break Err(describe_error(err));
                    }
                    batch.clear();
                }
            }
            TAG_END => {
                let text_bytes = match read_u64(reader) {
                    Ok(bytes) => bytes,
                    Err(err) => break Err(err),
                };
                if !batch.is_empty() {
                    if cancel.load(Ordering::Relaxed) {
                        break Err("Import cancelled.".into());
                    }
                    if let Err(err) = copy.send(batch.as_slice()).await {
                        break Err(describe_error(err));
                    }
                }
                break Ok(text_bytes);
            }
            _ => break Err("The import chunk has a broken COPY payload.".into()),
        }
    };
    match outcome {
        Ok(text_bytes) => match copy.finish().await {
            Ok(_) => Ok(text_bytes),
            Err(err) => Err(statement_failure(index, line, describe_error(err))),
        },
        Err(message) => {
            let _ = copy.abort(message.clone()).await;
            Err(statement_failure(index, line, message))
        }
    }
}

async fn close_conns(conns: Vec<Conn>) {
    futures_util::future::join_all(conns.into_iter().map(Conn::close)).await;
}

struct RunCtx<'a, Report> {
    preamble: &'a [String],
    cancel: &'a AtomicBool,
    error: &'a Mutex<Option<LoadSqlError>>,
    progress: &'a Mutex<ProgressState<Report>>,
    skip_completed: &'a HashSet<usize>,
    /// Whether the coordinator is inside a transaction the file opened, which batches must not commit or roll back.
    in_tx: AtomicBool,
}

struct ProgressState<Report> {
    statements: u64,
    bytes: u64,
    total_bytes: u64,
    report: Report,
    track: Option<LoadTrack>,
}

fn finish_phase1<Report: FnMut(LoadProgress)>(
    progress: &Mutex<ProgressState<Report>>,
    total_bytes: u64,
) {
    let mut progress = lock(progress);
    progress.total_bytes = total_bytes;
    let snapshot = LoadProgress {
        statements: progress.statements,
        bytes: progress.bytes,
        total_bytes: progress.total_bytes,
        statement_index: None,
    };
    (progress.report)(snapshot);
}

fn note_pre_progress<Report: FnMut(LoadProgress)>(
    progress: &Mutex<ProgressState<Report>>,
    bytes: u64,
    index: usize,
) {
    let mut progress = lock(progress);
    progress.statements += 1;
    progress.bytes += bytes;
    let snapshot = LoadProgress {
        statements: progress.statements,
        bytes: progress.bytes,
        total_bytes: progress.total_bytes,
        statement_index: Some(index),
    };
    (progress.report)(snapshot);
}

fn note_progress<Report: FnMut(LoadProgress)>(
    progress: &Mutex<ProgressState<Report>>,
    bytes: u64,
    index: usize,
    worker: usize,
) {
    let mut progress = lock(progress);
    progress.statements += 1;
    progress.bytes += bytes;
    if let Some(track) = &progress.track {
        track.bump(worker);
    }
    let snapshot = LoadProgress {
        statements: progress.statements,
        bytes: progress.bytes,
        total_bytes: progress.total_bytes,
        statement_index: Some(index),
    };
    (progress.report)(snapshot);
}

fn note_progress_batch<Report: FnMut(LoadProgress)>(
    progress: &Mutex<ProgressState<Report>>,
    batch: ProgressBatch,
) {
    if batch.indexes.is_empty() {
        return;
    }
    let mut progress = lock(progress);
    for (index, bytes) in batch.indexes {
        progress.statements += 1;
        progress.bytes += bytes;
        if let Some(track) = &progress.track {
            track.bump(batch.worker);
        }
        let snapshot = LoadProgress {
            statements: progress.statements,
            bytes: progress.bytes,
            total_bytes: progress.total_bytes,
            statement_index: Some(index),
        };
        (progress.report)(snapshot);
    }
}

fn note_post_progress<Report: FnMut(LoadProgress)>(
    progress: &Mutex<ProgressState<Report>>,
    bytes: u64,
    index: usize,
) {
    let mut progress = lock(progress);
    progress.statements += 1;
    progress.bytes += bytes;
    let snapshot = LoadProgress {
        statements: progress.statements,
        bytes: progress.bytes,
        total_bytes: progress.total_bytes,
        statement_index: Some(index),
    };
    (progress.report)(snapshot);
}

fn fail(
    error: &Mutex<Option<LoadSqlError>>,
    cancel: &AtomicBool,
    failure: impl Into<LoadSqlError>,
) {
    let failure = failure.into();
    let mut slot = lock(error);
    match &mut *slot {
        None => *slot = Some(failure),
        Some(existing) => existing.absorb(failure),
    }
    cancel.store(true, Ordering::Relaxed);
}

fn stop_requested(
    error: &Mutex<Option<LoadSqlError>>,
    cancel: &AtomicBool,
    line: Option<usize>,
) -> bool {
    if lock(error).is_some() {
        return true;
    }
    if !cancel.load(Ordering::Relaxed) {
        return false;
    }
    let message = match line {
        Some(line) => format!("Line {line}: Import cancelled."),
        None => "Import cancelled.".to_string(),
    };
    fail(error, cancel, message);
    true
}

fn take_error(
    error: &Mutex<Option<LoadSqlError>>,
    cancel: &AtomicBool,
) -> Result<(), LoadSqlError> {
    if let Some(failure) = lock(error).take() {
        return Err(failure);
    }
    if cancel.load(Ordering::Relaxed) {
        return Err("Import cancelled.".to_string().into());
    }
    Ok(())
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}

fn line_error(line: usize, message: impl Into<String>) -> String {
    let message = message.into();
    if message.starts_with("Line ") {
        message
    } else {
        format!("Line {line}: {message}")
    }
}

/// A lost connection stays its own variant. Every other statement error keeps
/// the `Line N:` text.
fn statement_failure(index: usize, line: usize, message: String) -> LoadSqlError {
    if is_connection_lost(&message) {
        LoadSqlError::LostConnection(LoadLostConnection {
            in_flight: vec![index],
            source: message,
        })
    } else {
        LoadSqlError::Failed(line_error(line, message))
    }
}

fn expand_preamble(
    driver: Driver,
    preamble: impl IntoIterator<Item = impl AsRef<str>>,
) -> Vec<String> {
    let mut out = Vec::new();
    for piece in preamble {
        let mut splitter = Splitter::new(driver);
        let mut statements = Vec::new();
        for line in piece.as_ref().split_inclusive('\n') {
            splitter.push_line(line, &mut statements);
        }
        splitter.finish(&mut statements);
        out.extend(statements.into_iter().map(|statement| statement.sql));
    }
    out
}

/// Balances per-table [`DataChunk`]s across up to `workers` threads by weighting
/// each table by its lines to import (`lines`, with `statements`, `bytes`, and
/// file appearance order as tie-breakers). Heaviest tables are assigned first to
/// the worker thread with the fewest assigned lines so far.
fn assign_workers(chunks: &[DataChunk], workers: usize) -> Vec<Vec<DataChunk>> {
    if chunks.is_empty() {
        return Vec::new();
    }
    let slots = workers.max(1).min(chunks.len());
    if slots <= 1 {
        return vec![chunks.to_vec()];
    }

    struct TableWeight {
        first_idx: usize,
        lines: u64,
        statements: u64,
        bytes: u64,
        chunk_indices: Vec<usize>,
    }

    let mut tables: Vec<TableWeight> = Vec::new();
    let mut table_pos: HashMap<usize, usize> = HashMap::new();
    for (idx, chunk) in chunks.iter().enumerate() {
        if let Some(&pos) = table_pos.get(&chunk.table_id) {
            let entry = &mut tables[pos];
            entry.lines += chunk.lines;
            entry.statements += chunk.statements;
            entry.bytes += chunk.bytes;
            entry.chunk_indices.push(idx);
        } else {
            let pos = tables.len();
            table_pos.insert(chunk.table_id, pos);
            tables.push(TableWeight {
                first_idx: idx,
                lines: chunk.lines,
                statements: chunk.statements,
                bytes: chunk.bytes,
                chunk_indices: vec![idx],
            });
        }
    }

    let unique_tables = tables.len();
    if unique_tables < slots && chunks.len() > unique_tables {
        let mut order: Vec<usize> = (0..chunks.len()).collect();
        order.sort_by(|&a, &b| {
            (chunks[b].lines, chunks[b].statements, chunks[b].bytes)
                .cmp(&(chunks[a].lines, chunks[a].statements, chunks[a].bytes))
                .then_with(|| a.cmp(&b))
        });
        let mut groups = vec![Vec::new(); slots];
        let mut loads = vec![(0u64, 0u64, 0u64); slots];
        for idx in order {
            let chunk = &chunks[idx];
            let worker = loads
                .iter()
                .enumerate()
                .min_by_key(|&(w, &(lines, statements, bytes))| (lines, statements, bytes, w))
                .map(|(w, _)| w)
                .unwrap_or(0);
            loads[worker].0 += chunk.lines;
            loads[worker].1 += chunk.statements;
            loads[worker].2 += chunk.bytes;
            groups[worker].push(chunk.clone());
        }
        return groups;
    }

    let mut order: Vec<usize> = (0..tables.len()).collect();
    order.sort_by(|&a, &b| {
        (tables[b].lines, tables[b].statements, tables[b].bytes)
            .cmp(&(tables[a].lines, tables[a].statements, tables[a].bytes))
            .then_with(|| tables[a].first_idx.cmp(&tables[b].first_idx))
    });
    let mut groups = vec![Vec::new(); slots];
    let mut loads = vec![(0u64, 0u64, 0u64); slots];
    for t_idx in order {
        let table = &tables[t_idx];
        let worker = loads
            .iter()
            .enumerate()
            .min_by_key(|&(w, &(lines, statements, bytes))| (lines, statements, bytes, w))
            .map(|(w, _)| w)
            .unwrap_or(0);
        loads[worker].0 += table.lines;
        loads[worker].1 += table.statements;
        loads[worker].2 += table.bytes;
        for &chunk_idx in &table.chunk_indices {
            groups[worker].push(chunks[chunk_idx].clone());
        }
    }
    groups
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Pre,
    Data,
    Post,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    Pre,
    Data,
    Post,
    Skip,
}

#[cfg(test)]
impl Decision {
    fn phase(self) -> Option<Phase> {
        match self {
            Decision::Pre => Some(Phase::Pre),
            Decision::Data => Some(Phase::Data),
            Decision::Post => Some(Phase::Post),
            Decision::Skip => None,
        }
    }
}

#[cfg(test)]
fn decide_phase(sql: &str, seen_data: bool, workers: usize) -> Decision {
    decide_phase_with_table(sql, seen_data, workers).0
}

fn decide_phase_with_table(
    sql: &str,
    seen_data: bool,
    workers: usize,
) -> (Decision, Option<String>) {
    let Some(first) = Words::new(sql).next() else {
        return (contextual(seen_data), None);
    };
    if first.eq_ignore_ascii_case("INSERT") || first.eq_ignore_ascii_case("REPLACE") {
        let table = data_target_table(sql);
        if table.as_deref() == Some("sqlite_sequence") {
            return (Decision::Post, None);
        }
        return (Decision::Data, table);
    }
    if first.eq_ignore_ascii_case("COPY") && is_copy_from_stdin(sql) {
        let table = (workers > 1).then(|| data_target_table(sql)).flatten();
        return (Decision::Data, table);
    }
    if first.eq_ignore_ascii_case("BEGIN") {
        return (Decision::Pre, None);
    }
    if first.eq_ignore_ascii_case("START") && is_start_transaction(sql) {
        return (Decision::Pre, None);
    }
    if first.eq_ignore_ascii_case("COMMIT") || first.eq_ignore_ascii_case("ROLLBACK") {
        return (Decision::Post, None);
    }
    if first.eq_ignore_ascii_case("END") && is_bare_end(sql) {
        return (Decision::Post, None);
    }
    if first.eq_ignore_ascii_case("DROP") {
        return (drop_phase(sql, seen_data), None);
    }
    if first.eq_ignore_ascii_case("TRUNCATE") {
        return (Decision::Pre, None);
    }
    if first.eq_ignore_ascii_case("COMMENT") {
        return (comment_phase(sql), None);
    }
    if first.eq_ignore_ascii_case("LOCK") {
        return (lock_tables(sql, false, seen_data, workers), None);
    }
    if first.eq_ignore_ascii_case("UNLOCK") {
        return (lock_tables(sql, true, seen_data, workers), None);
    }
    if first.eq_ignore_ascii_case("SET")
        || first.eq_ignore_ascii_case("USE")
        || first.eq_ignore_ascii_case("PRAGMA")
    {
        return (contextual(seen_data), None);
    }
    if first.eq_ignore_ascii_case("SELECT") && is_setval(sql) {
        return (Decision::Post, None);
    }
    if first.eq_ignore_ascii_case("CREATE") {
        return (create_phase(sql, seen_data), None);
    }
    if first.eq_ignore_ascii_case("ALTER") {
        return (alter_phase(sql, seen_data), None);
    }
    (contextual(seen_data), None)
}

/// Dropping an object that phase 3 may create again keeps its place before that `CREATE`.
fn drop_phase(sql: &str, seen_data: bool) -> Decision {
    const LATER: [&str; 9] = [
        "VIEW",
        "MATERIALIZED",
        "TRIGGER",
        "INDEX",
        "FUNCTION",
        "PROCEDURE",
        "EVENT",
        "RULE",
        "POLICY",
    ];
    let mut words = Words::new(sql);
    words.next();
    match words.next() {
        Some(kind) if LATER.iter().any(|later| kind.eq_ignore_ascii_case(later)) => {
            contextual(seen_data)
        }
        _ => Decision::Pre,
    }
}

fn contextual(seen_data: bool) -> Decision {
    if seen_data {
        Decision::Post
    } else {
        Decision::Pre
    }
}

fn comment_phase(sql: &str) -> Decision {
    let mut words = Words::new(sql);
    if !words
        .next()
        .is_some_and(|w| w.eq_ignore_ascii_case("COMMENT"))
    {
        return Decision::Pre;
    }
    if !words.next().is_some_and(|w| w.eq_ignore_ascii_case("ON")) {
        return Decision::Pre;
    }
    if let Some(target) = words.next() {
        if target.eq_ignore_ascii_case("VIEW")
            || target.eq_ignore_ascii_case("MATERIALIZED")
            || target.eq_ignore_ascii_case("COLUMN")
            || target.eq_ignore_ascii_case("INDEX")
            || target.eq_ignore_ascii_case("TRIGGER")
            || target.eq_ignore_ascii_case("CONSTRAINT")
            || target.eq_ignore_ascii_case("RULE")
            || target.eq_ignore_ascii_case("POLICY")
        {
            return Decision::Post;
        }
    }
    Decision::Pre
}

fn is_start_transaction(sql: &str) -> bool {
    let mut words = Words::new(sql);
    words
        .next()
        .is_some_and(|w| w.eq_ignore_ascii_case("START"))
        && words
            .next()
            .is_some_and(|w| w.eq_ignore_ascii_case("TRANSACTION"))
}

/// SQLite accepts `END` as the closing half of `BEGIN`.
fn is_bare_end(sql: &str) -> bool {
    let mut words = Words::new(sql);
    if !words.next().is_some_and(|w| w.eq_ignore_ascii_case("END")) {
        return false;
    }
    match words.next() {
        None => true,
        Some(w) => w.eq_ignore_ascii_case("TRANSACTION") || w.eq_ignore_ascii_case("WORK"),
    }
}

fn lock_tables(sql: &str, unlock: bool, seen_data: bool, workers: usize) -> Decision {
    let mut words = Words::new(sql);
    let first = words.next();
    let second = words.next();
    let matched = if unlock {
        first.is_some_and(|w| w.eq_ignore_ascii_case("UNLOCK"))
            && second.is_some_and(|w| w.eq_ignore_ascii_case("TABLES"))
    } else {
        first.is_some_and(|w| w.eq_ignore_ascii_case("LOCK"))
            && second.is_some_and(|w| w.eq_ignore_ascii_case("TABLES"))
    };
    if !matched {
        return contextual(seen_data);
    }
    if workers > 1 {
        Decision::Skip
    } else if unlock {
        Decision::Post
    } else {
        Decision::Pre
    }
}

fn is_setval(sql: &str) -> bool {
    let mut words = Words::new(sql);
    if !words
        .next()
        .is_some_and(|w| w.eq_ignore_ascii_case("SELECT"))
    {
        return false;
    }
    match words.next() {
        Some(word) if word.eq_ignore_ascii_case("SETVAL") => true,
        Some(word) => word.len() > 7 && word[word.len() - 7..].eq_ignore_ascii_case(".SETVAL"),
        None => false,
    }
}

fn create_phase(sql: &str, seen_data: bool) -> Decision {
    let mut words = Words::new(sql);
    if !words
        .next()
        .is_some_and(|w| w.eq_ignore_ascii_case("CREATE"))
    {
        return contextual(seen_data);
    }
    for word in words {
        if is_create_modifier(word) {
            continue;
        }
        if word.eq_ignore_ascii_case("TABLE")
            || word.eq_ignore_ascii_case("SEQUENCE")
            || word.eq_ignore_ascii_case("TYPE")
            || word.eq_ignore_ascii_case("DOMAIN")
            || word.eq_ignore_ascii_case("EXTENSION")
            || word.eq_ignore_ascii_case("SCHEMA")
            || word.eq_ignore_ascii_case("DATABASE")
        {
            return Decision::Pre;
        }
        if word.eq_ignore_ascii_case("INDEX")
            || word.eq_ignore_ascii_case("VIEW")
            || word.eq_ignore_ascii_case("TRIGGER")
            || word.eq_ignore_ascii_case("MATERIALIZED")
        {
            return Decision::Post;
        }
        return contextual(seen_data);
    }
    contextual(seen_data)
}

fn is_create_modifier(word: &str) -> bool {
    word.eq_ignore_ascii_case("OR")
        || word.eq_ignore_ascii_case("REPLACE")
        || word.eq_ignore_ascii_case("UNIQUE")
        || word.eq_ignore_ascii_case("TEMP")
        || word.eq_ignore_ascii_case("TEMPORARY")
        || word.eq_ignore_ascii_case("UNLOGGED")
        || word.eq_ignore_ascii_case("GLOBAL")
        || word.eq_ignore_ascii_case("LOCAL")
        || word.eq_ignore_ascii_case("IF")
        || word.eq_ignore_ascii_case("NOT")
        || word.eq_ignore_ascii_case("EXISTS")
        || word.eq_ignore_ascii_case("CONCURRENTLY")
        || word.eq_ignore_ascii_case("CONSTRAINT")
}

fn alter_phase(sql: &str, seen_data: bool) -> Decision {
    let mut iter = Words::new(sql);
    let Some(first) = iter.next() else {
        return contextual(seen_data);
    };
    if !first.eq_ignore_ascii_case("ALTER") {
        return contextual(seen_data);
    }
    let Some(second) = iter.next() else {
        return contextual(seen_data);
    };
    if second.eq_ignore_ascii_case("SEQUENCE") {
        return Decision::Pre;
    }
    if second.eq_ignore_ascii_case("VIEW") || second.eq_ignore_ascii_case("MATERIALIZED") {
        return Decision::Post;
    }
    let mut buf = [""; 64];
    buf[0] = first;
    buf[1] = second;
    let mut len = 2;
    let mut matched_post = false;
    loop {
        while len < buf.len() {
            let Some(word) = iter.next() else {
                break;
            };
            buf[len] = word;
            len += 1;
        }
        let words = &buf[..len];
        if pair(words, "DISABLE", "KEYS")
            || pair(words, "DROP", "CONSTRAINT")
            || triple(words, "DROP", "FOREIGN", "KEY")
            || pair(words, "SET", "DEFAULT")
            || pair(words, "ADD", "GENERATED")
            || drops_key(words)
        {
            return Decision::Pre;
        }
        if !matched_post
            && (pair(words, "ENABLE", "KEYS")
                || pair(words, "ADD", "CONSTRAINT")
                || triple(words, "ADD", "FOREIGN", "KEY")
                || adds_key(words)
                || toggles_trigger(words))
        {
            matched_post = true;
        }
        if len < buf.len() {
            break;
        }
        buf.copy_within(58..64, 0);
        len = 6;
    }
    if matched_post {
        return Decision::Post;
    }
    contextual(seen_data)
}

fn pair(words: &[&str], left: &str, right: &str) -> bool {
    words
        .windows(2)
        .any(|window| window[0].eq_ignore_ascii_case(left) && window[1].eq_ignore_ascii_case(right))
}

fn triple(words: &[&str], a: &str, b: &str, c: &str) -> bool {
    words.windows(3).any(|window| {
        window[0].eq_ignore_ascii_case(a)
            && window[1].eq_ignore_ascii_case(b)
            && window[2].eq_ignore_ascii_case(c)
    })
}

fn drops_key(words: &[&str]) -> bool {
    words.iter().enumerate().any(|(index, word)| {
        if !word.eq_ignore_ascii_case("DROP") {
            return false;
        }
        let rest = &words[index + 1..];
        match rest.first().copied() {
            Some(w) if w.eq_ignore_ascii_case("KEY") || w.eq_ignore_ascii_case("INDEX") => true,
            Some(w) if w.eq_ignore_ascii_case("PRIMARY") => {
                rest.get(1).is_some_and(|k| k.eq_ignore_ascii_case("KEY"))
            }
            _ => false,
        }
    })
}

fn adds_key(words: &[&str]) -> bool {
    for (index, word) in words.iter().enumerate() {
        if !word.eq_ignore_ascii_case("ADD") {
            continue;
        }
        let mut cursor = index + 1;
        while cursor < words.len()
            && (words[cursor].eq_ignore_ascii_case("UNIQUE")
                || words[cursor].eq_ignore_ascii_case("FULLTEXT")
                || words[cursor].eq_ignore_ascii_case("SPATIAL"))
        {
            cursor += 1;
        }
        match words.get(cursor).copied() {
            Some(w) if w.eq_ignore_ascii_case("KEY") || w.eq_ignore_ascii_case("INDEX") => {
                return true
            }
            Some(w)
                if w.eq_ignore_ascii_case("PRIMARY")
                    && words
                        .get(cursor + 1)
                        .is_some_and(|k| k.eq_ignore_ascii_case("KEY")) =>
            {
                return true
            }
            _ => {}
        }
    }
    false
}

fn toggles_trigger(words: &[&str]) -> bool {
    words.iter().enumerate().any(|(index, word)| {
        (word.eq_ignore_ascii_case("ENABLE") || word.eq_ignore_ascii_case("DISABLE"))
            && words[index + 1..]
                .iter()
                .take(3)
                .any(|next| next.eq_ignore_ascii_case("TRIGGER"))
    })
}

/// Leading keywords of a statement. MySQL executable hints stay visible, and
/// quoted text is not treated as keywords.
struct Words<'a> {
    sql: &'a str,
    index: usize,
}

impl<'a> Words<'a> {
    fn new(sql: &'a str) -> Self {
        Self { sql, index: 0 }
    }
}

impl<'a> Iterator for Words<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        let bytes = self.sql.as_bytes();
        loop {
            if self.index >= bytes.len() {
                return None;
            }
            let current = bytes[self.index];
            if current.is_ascii_whitespace() {
                self.index += 1;
                continue;
            }
            if current == b'/' && bytes.get(self.index + 1) == Some(&b'*') {
                self.skip_comment();
                continue;
            }
            if current == b'-' && bytes.get(self.index + 1) == Some(&b'-') {
                self.index += 2;
                match memchr::memchr(b'\n', &bytes[self.index..]) {
                    Some(pos) => self.index += pos + 1,
                    None => self.index = bytes.len(),
                }
                continue;
            }
            if matches!(current, b'\'' | b'"' | b'`') {
                self.skip_quote(current);
                continue;
            }
            if current == b'$' && self.skip_dollar() {
                continue;
            }
            if is_word_byte(current) {
                let start = self.index;
                self.index += 1;
                while self.index < bytes.len()
                    && (is_word_byte(bytes[self.index]) || bytes[self.index] == b'.')
                {
                    self.index += 1;
                }
                return Some(&self.sql[start..self.index]);
            }
            self.index += 1;
        }
    }
}

impl Words<'_> {
    fn skip_comment(&mut self) {
        let bytes = self.sql.as_bytes();
        let hint = matches!(bytes.get(self.index + 2), Some(b'!' | b'+'));
        if hint {
            self.index += 3;
            if bytes.get(self.index.wrapping_sub(1)) == Some(&b'!') {
                while self.index < bytes.len() && bytes[self.index].is_ascii_digit() {
                    self.index += 1;
                }
            }
            return;
        }
        self.index = (self.index + 2).min(bytes.len());
        match memchr::memmem::find(&bytes[self.index..], b"*/") {
            Some(pos) => self.index += pos + 2,
            None => self.index = bytes.len(),
        }
    }

    fn skip_quote(&mut self, quote: u8) {
        let bytes = self.sql.as_bytes();
        self.index += 1;
        while let Some(pos) = memchr::memchr2(quote, b'\\', &bytes[self.index..]) {
            self.index += pos;
            if bytes[self.index] == b'\\' {
                self.index = (self.index + 2).min(bytes.len());
                continue;
            }
            if bytes.get(self.index + 1) == Some(&quote) {
                self.index += 2;
                continue;
            }
            self.index += 1;
            return;
        }
        self.index = bytes.len();
    }

    fn skip_dollar(&mut self) -> bool {
        let bytes = self.sql.as_bytes();
        let start = self.index;
        let mut end = start + 1;
        while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
            end += 1;
        }
        if end >= bytes.len() || bytes[end] != b'$' {
            return false;
        }
        if end > start + 1 && bytes[start + 1].is_ascii_digit() {
            return false;
        }
        let tag = &bytes[start..=end];
        self.index = end + 1;
        match memchr::memmem::find(&bytes[self.index..], tag) {
            Some(found) => self.index += found + tag.len(),
            None => self.index = self.sql.len(),
        }
        true
    }
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

struct Stream<R> {
    reader: R,
    splitter: Splitter,
    pending: VecDeque<Statement>,
    emitted: Vec<Statement>,
    line: Vec<u8>,
    done: bool,
}

impl<R: BufRead> Stream<R> {
    fn new(reader: R, driver: Driver) -> Self {
        Self {
            reader,
            splitter: Splitter::new(driver),
            pending: VecDeque::new(),
            emitted: Vec::new(),
            line: Vec::new(),
            done: false,
        }
    }

    fn recycle(&mut self, sql: String) {
        self.splitter.recycle(sql);
    }

    fn next_statement(&mut self, cancel: &AtomicBool) -> Result<Option<Statement>, String> {
        loop {
            if let Some(statement) = self.pending.pop_front() {
                return Ok(Some(statement));
            }
            if self.done {
                return Ok(None);
            }
            if cancel.load(Ordering::Relaxed) {
                return Err("Import cancelled.".into());
            }
            self.line.clear();
            let read = self
                .reader
                .read_until(b'\n', &mut self.line)
                .map_err(|err| format!("Could not read the SQL file: {err}"))?;
            if read == 0 {
                self.emitted.clear();
                self.splitter.finish(&mut self.emitted);
                self.pending.extend(self.emitted.drain(..));
                self.done = true;
                continue;
            }
            if cancel.load(Ordering::Relaxed) {
                return Err("Import cancelled.".into());
            }
            self.emitted.clear();
            self.splitter.push_bytes(&self.line, &mut self.emitted)?;
            self.pending.extend(self.emitted.drain(..));
        }
    }

    /// Reads COPY data lines through the closing `\.` without splitting them.
    /// `on_data` receives batched rows as they are sent to Postgres, with `\r` removed
    /// and a `\n` terminator. The byte count includes the original bytes of
    /// those lines and of the `\.` line, which is returned unchanged.
    fn consume_copy(
        &mut self,
        cancel: &AtomicBool,
        mut on_data: impl FnMut(&[u8]) -> Result<(), String>,
    ) -> Result<(u64, u64, Vec<u8>), String> {
        let mut payload_bytes = 0u64;
        let mut data_lines = 0u64;
        let mut batch = Vec::with_capacity(COPY_SEND);
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err("Import cancelled.".into());
            }
            let buf = self
                .reader
                .fill_buf()
                .map_err(|err| format!("Could not read the SQL file: {err}"))?;
            if buf.is_empty() {
                return Err("The file ended before the COPY data did.".into());
            }
            let mut consumed = 0usize;
            let mut lines_in_buf = 0usize;
            let mut flush_batch = false;
            let mut end_line: Option<Vec<u8>> = None;
            while let Some(pos) = memchr::memchr(b'\n', &buf[consumed..]) {
                let line_end = consumed + pos + 1;
                let raw_line = &buf[consumed..line_end];
                consumed = line_end;
                lines_in_buf += 1;
                payload_bytes += raw_line.len() as u64;
                let mut content = &raw_line[..raw_line.len() - 1];
                if let Some(stripped) = content.strip_suffix(b"\r") {
                    content = stripped;
                }
                if content == b"\\." {
                    end_line = Some(raw_line.to_vec());
                    break;
                }
                data_lines += 1;
                batch.extend_from_slice(content);
                batch.push(b'\n');
                if batch.len() >= COPY_SEND {
                    flush_batch = true;
                    break;
                }
            }
            if consumed > 0 {
                self.reader.consume(consumed);
                self.splitter.skip_lines(lines_in_buf);
                if let Some(closing) = end_line {
                    if !batch.is_empty() {
                        on_data(&batch)?;
                    }
                    return Ok((payload_bytes, data_lines, closing));
                }
                if flush_batch {
                    on_data(&batch)?;
                    batch.clear();
                }
                continue;
            }
            self.line.clear();
            let read = self
                .reader
                .read_until(b'\n', &mut self.line)
                .map_err(|err| format!("Could not read the SQL file: {err}"))?;
            if read == 0 {
                return Err("The file ended before the COPY data did.".into());
            }
            self.splitter.skip_lines(1);
            payload_bytes += self.line.len() as u64;
            let mut content = self.line.as_slice();
            if let Some(stripped) = content.strip_suffix(b"\n") {
                content = stripped;
            }
            if let Some(stripped) = content.strip_suffix(b"\r") {
                content = stripped;
            }
            if content == b"\\." {
                if !batch.is_empty() {
                    on_data(&batch)?;
                }
                return Ok((payload_bytes, data_lines, self.line.clone()));
            }
            data_lines += 1;
            batch.extend_from_slice(content);
            batch.push(b'\n');
            if batch.len() >= COPY_SEND {
                on_data(&batch)?;
                batch.clear();
            }
        }
    }
}

struct Bin {
    table_id: usize,
    lines: u64,
    bytes: u64,
    statements: u64,
    file: Option<ChunkFile>,
}

impl Bin {
    fn with_table(table_id: usize) -> Self {
        Self {
            table_id,
            lines: 0,
            bytes: 0,
            statements: 0,
            file: None,
        }
    }
}

fn select_bin(bins: &mut Vec<Bin>, workers: usize, target: u64) -> usize {
    let workers = workers.max(1);
    while bins.len() < workers {
        let table_id = bins.len();
        bins.push(Bin::with_table(table_id));
    }
    let idx = bins
        .iter()
        .enumerate()
        .min_by_key(|(_, bin)| (bin.lines, bin.statements, bin.bytes))
        .map(|(idx, _)| idx)
        .unwrap_or(0);
    if bins[idx].bytes >= target {
        let table_id = bins.len();
        bins.push(Bin::with_table(table_id));
        bins.len() - 1
    } else {
        idx
    }
}

/**
 * Phase 1's copy of the data statements, written to chunks for phase 2.
 *
 * With more than one worker, each table's statements go to that table's own
 * chunk, so one connection loads a table in file order and connections never
 * contend on one table's auto-increment or index locks. With `split_tables`,
 * a large table is cut into several chunks that workers may share.
 */
struct Spool {
    bins: Vec<Bin>,
    tables: HashMap<String, usize>,
    last: Option<usize>,
    workers: usize,
    split_tables: bool,
}

impl Spool {
    fn new(workers: usize, split_tables: bool) -> Self {
        Self {
            bins: Vec::new(),
            tables: HashMap::new(),
            last: None,
            workers: workers.max(1),
            split_tables,
        }
    }

    fn bin_for(&mut self, table: Option<String>) -> usize {
        if self.workers <= 1 {
            return select_bin(&mut self.bins, 1, TABLE_CHUNK_BYTES);
        }
        let Some(table) = table else {
            let idx = self.bins.len();
            self.bins.push(Bin::with_table(idx));
            return idx;
        };
        let Some(slot) = self.tables.get_mut(table.as_str()) else {
            let idx = self.bins.len();
            self.bins.push(Bin::with_table(idx));
            self.tables.insert(table, idx);
            return idx;
        };
        let bin = &self.bins[*slot];
        let full = bin.bytes >= TABLE_CHUNK_BYTES || bin.statements >= TABLE_CHUNK_STMTS;
        if !self.split_tables || !full {
            return *slot;
        }
        let table_id = bin.table_id;
        *slot = self.bins.len();
        self.bins.push(Bin::with_table(table_id));
        *slot
    }

    /// The bin for `table`, with its file open. Flushes the last one used, so only one disk chunk holds a buffer at a time.
    fn open(&mut self, table: Option<String>) -> Result<usize, String> {
        let idx = self.bin_for(table);
        if let Some(prev) = self.last.replace(idx) {
            if prev != idx {
                if let Some(file) = self.bins[prev].file.as_mut() {
                    file.suspend_disk()?;
                }
            }
        }
        self.bins[idx].file.get_or_insert_with(ChunkFile::new);
        Ok(idx)
    }

    /// Spools one data statement and returns its size.
    fn push_sql(
        &mut self,
        index: usize,
        stmt: &Statement,
        table: Option<String>,
        temps: &mut TempFiles,
    ) -> Result<u64, String> {
        let idx = self.open(table)?;
        let bin = &mut self.bins[idx];
        let file = bin.file.as_mut().expect("data chunk");
        file.reserve(stmt.sql.len() + 32, temps)?;
        write_sql_record(file, index, stmt.line, &stmt.sql)?;
        let text_bytes = stmt.sql.len() as u64;
        bin.lines += stmt.line_span();
        bin.bytes += text_bytes;
        bin.statements += 1;
        Ok(text_bytes)
    }

    /// Spools a `COPY ... FROM stdin` and the rows `stream` holds after it, and returns their size.
    fn push_copy<R: BufRead>(
        &mut self,
        index: usize,
        stmt: &Statement,
        stream: &mut Stream<R>,
        cancel: &AtomicBool,
        temps: &mut TempFiles,
    ) -> Result<u64, LoadSqlError> {
        let table = (self.workers > 1)
            .then(|| data_target_table(&stmt.sql))
            .flatten();
        let idx = self.open(table)?;
        let bin = &mut self.bins[idx];
        let file = bin.file.as_mut().expect("data chunk");
        file.reserve(stmt.sql.len() + 32, temps)?;
        write_copy_start(file, index, stmt.line, &stmt.sql)?;
        let (payload_bytes, copy_lines, _) = stream
            .consume_copy(cancel, |chunk| {
                file.reserve(chunk.len() + 16, temps)?;
                write_copy_data(file, chunk)
            })
            .map_err(|err| LoadSqlError::from(line_error(stmt.line, err)))?;
        let text_bytes = stmt.sql.len() as u64 + payload_bytes;
        write_copy_end(file, text_bytes)?;
        bin.lines += stmt.line_span() + copy_lines;
        bin.bytes += text_bytes;
        bin.statements += 1;
        Ok(text_bytes)
    }

    /// The finished chunks, and how many statements each worker starts with.
    fn finish(self) -> Result<(Vec<DataChunk>, Vec<u64>), String> {
        finish_bins(self.bins, self.workers)
    }
}

fn is_insert_modifier(word: &str) -> bool {
    word.eq_ignore_ascii_case("INSERT")
        || word.eq_ignore_ascii_case("REPLACE")
        || word.eq_ignore_ascii_case("COPY")
        || word.eq_ignore_ascii_case("OR")
        || word.eq_ignore_ascii_case("ABORT")
        || word.eq_ignore_ascii_case("FAIL")
        || word.eq_ignore_ascii_case("ROLLBACK")
        || word.eq_ignore_ascii_case("LOW_PRIORITY")
        || word.eq_ignore_ascii_case("DELAYED")
        || word.eq_ignore_ascii_case("HIGH_PRIORITY")
        || word.eq_ignore_ascii_case("IGNORE")
        || word.eq_ignore_ascii_case("INTO")
        || word.eq_ignore_ascii_case("ONLY")
}

fn data_target_table(sql: &str) -> Option<String> {
    let first = Words::new(sql).next()?;
    if first.eq_ignore_ascii_case("INSERT")
        || first.eq_ignore_ascii_case("REPLACE")
        || first.eq_ignore_ascii_case("COPY")
    {
        let table = normalize_table_ident(sql);
        if !table.is_empty() {
            return Some(table);
        }
    }
    None
}

fn read_ident_segment(bytes: &[u8], i: &mut usize) -> Option<String> {
    if *i >= bytes.len() {
        return None;
    }
    if matches!(bytes[*i], b'`' | b'"' | b'[') {
        let open = bytes[*i];
        let close = if open == b'[' { b']' } else { open };
        *i += 1;
        let start = *i;
        if let Some(pos) = memchr::memchr(close, &bytes[start..]) {
            let close_idx = start + pos;
            if open == b'[' || bytes.get(close_idx + 1) != Some(&close) {
                *i = close_idx + 1;
                if close_idx == start {
                    return None;
                }
                return Some(
                    std::str::from_utf8(&bytes[start..close_idx])
                        .unwrap_or("")
                        .to_ascii_lowercase(),
                );
            }
        }
        let mut ident = String::new();
        while *i < bytes.len() {
            if bytes[*i] == close {
                *i += 1;
                if open != b'[' && *i < bytes.len() && bytes[*i] == close {
                    ident.push(close.to_ascii_lowercase() as char);
                    *i += 1;
                    continue;
                }
                break;
            }
            ident.push(bytes[*i].to_ascii_lowercase() as char);
            *i += 1;
        }
        return (!ident.is_empty()).then_some(ident);
    }
    if is_word_byte(bytes[*i]) {
        let start = *i;
        *i += 1;
        while *i < bytes.len() && is_word_byte(bytes[*i]) {
            *i += 1;
        }
        return Some(
            std::str::from_utf8(&bytes[start..*i])
                .unwrap_or("")
                .to_ascii_lowercase(),
        );
    }
    None
}

fn normalize_table_ident(sql: &str) -> String {
    // Support unquoted, quoted (`...`, "...", [...]), and mixed schema.table identifiers.
    let bytes = sql.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
            if bytes.get(i + 2) == Some(&b'!') {
                i += 3;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                continue;
            }
            i = (i + 2).min(bytes.len());
            match memchr::memmem::find(&bytes[i..], b"*/") {
                Some(pos) => i += pos + 2,
                None => i = bytes.len(),
            }
            continue;
        }
        if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
            i += 2;
            continue;
        }
        if bytes[i] == b'-' && bytes.get(i + 1) == Some(&b'-') {
            i += 2;
            match memchr::memchr(b'\n', &bytes[i..]) {
                Some(pos) => i += pos + 1,
                None => i = bytes.len(),
            }
            continue;
        }
        if is_word_byte(bytes[i]) {
            let start = i;
            let mut end = i + 1;
            while end < bytes.len() && is_word_byte(bytes[end]) {
                end += 1;
            }
            if is_insert_modifier(&sql[start..end]) {
                i = end;
                continue;
            }
        }
        if let Some((table, _)) = read_qualified_table_ident(&sql[i..]) {
            return table;
        }
        break;
    }
    String::new()
}

fn read_qualified_table_ident(sql: &str) -> Option<(String, usize)> {
    let bytes = sql.as_bytes();
    let mut i = 0;
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            return None;
        }
        if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
            if bytes.get(i + 2) == Some(&b'!') {
                i += 3;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                continue;
            }
            i = (i + 2).min(bytes.len());
            match memchr::memmem::find(&bytes[i..], b"*/") {
                Some(pos) => i += pos + 2,
                None => i = bytes.len(),
            }
            continue;
        }
        if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
            i += 2;
            continue;
        }
        if bytes[i] == b'-' && bytes.get(i + 1) == Some(&b'-') {
            i += 2;
            match memchr::memchr(b'\n', &bytes[i..]) {
                Some(pos) => i += pos + 1,
                None => i = bytes.len(),
            }
            continue;
        }
        break;
    }
    let mut last = read_ident_segment(bytes, &mut i)?;
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'.' {
            break;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let Some(next) = read_ident_segment(bytes, &mut i) else {
            break;
        };
        last = next;
    }
    Some((last, i))
}

fn has_top_level_comma(sql: &str) -> bool {
    let bytes = sql.as_bytes();
    let mut i = 0;
    let mut depth = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'/' && bytes.get(i + 1) == Some(&b'*') {
            i = (i + 2).min(bytes.len());
            match memchr::memmem::find(&bytes[i..], b"*/") {
                Some(pos) => i += pos + 2,
                None => break,
            }
            continue;
        }
        if b == b'-' && bytes.get(i + 1) == Some(&b'-') {
            i += 2;
            match memchr::memchr(b'\n', &bytes[i..]) {
                Some(pos) => i += pos + 1,
                None => break,
            }
            continue;
        }
        if matches!(b, b'\'' | b'"' | b'`') {
            i += 1;
            while let Some(pos) = memchr::memchr2(b, b'\\', &bytes[i..]) {
                i += pos;
                if bytes[i] == b'\\' {
                    i = (i + 2).min(bytes.len());
                    continue;
                }
                if bytes.get(i + 1) == Some(&b) {
                    i += 2;
                    continue;
                }
                i += 1;
                break;
            }
            continue;
        }
        if b == b'(' {
            depth += 1;
        } else if b == b')' {
            depth = depth.saturating_sub(1);
        } else if b == b',' && depth == 0 {
            return true;
        }
        i += 1;
    }
    false
}

/// Records both the defining table and any `REFERENCES <table_ident>` target
/// tables when a `CREATE TABLE` or `ALTER TABLE` statement introduces foreign
/// keys. In MySQL 8.0, once a foreign key links two tables in the data
/// dictionary, subsequent `DROP TABLE`, `CREATE TABLE`, or `ALTER TABLE`
/// statements on either table acquire metadata locks on both tables even when
/// `FOREIGN_KEY_CHECKS=0`, so they must execute as barriers on the coordinator.
fn collect_fk_tables(sql: &str, fk_tables: &mut HashSet<String>) {
    let mut head = Words::new(sql);
    let Some(first) = head.next() else {
        return;
    };
    if !first.eq_ignore_ascii_case("CREATE") && !first.eq_ignore_ascii_case("ALTER") {
        return;
    }
    let mut has_fk = false;
    let mut scan = Words::new(sql);
    while let Some(w) = scan.next() {
        if w.eq_ignore_ascii_case("REFERENCES") {
            has_fk = true;
            if let Some((ref_table, _)) = read_qualified_table_ident(&sql[scan.index..]) {
                if !ref_table.is_empty() {
                    fk_tables.insert(ref_table);
                }
            }
        } else if w.eq_ignore_ascii_case("FOREIGN") {
            has_fk = true;
        }
    }
    if has_fk {
        if let Some(own_table) = ddl_own_table(sql) {
            fk_tables.insert(own_table);
        }
    }
}

fn ddl_own_table(sql: &str) -> Option<String> {
    let mut words = Words::new(sql);
    let first = words.next()?;
    if first.eq_ignore_ascii_case("CREATE") {
        let mut word = words.next()?;
        if word.eq_ignore_ascii_case("UNLOGGED") {
            word = words.next()?;
        }
        if !word.eq_ignore_ascii_case("TABLE") {
            return None;
        }
        let mut after_kw = words.index;
        if let Some(w1) = words.next() {
            if w1.eq_ignore_ascii_case("IF") {
                if let Some(w2) = words.next() {
                    if w2.eq_ignore_ascii_case("NOT") {
                        if let Some(w3) = words.next() {
                            if w3.eq_ignore_ascii_case("EXISTS") {
                                after_kw = words.index;
                            }
                        }
                    }
                }
            }
        }
        let (table, _) = read_qualified_table_ident(&sql[after_kw..])?;
        return (!table.is_empty()).then_some(table);
    }
    if first.eq_ignore_ascii_case("ALTER") {
        let second = words.next()?;
        if !second.eq_ignore_ascii_case("TABLE") {
            return None;
        }
        let mut after_kw = words.index;
        while let Some(w) = words.next() {
            if w.eq_ignore_ascii_case("IF")
                || w.eq_ignore_ascii_case("EXISTS")
                || w.eq_ignore_ascii_case("ONLY")
            {
                after_kw = words.index;
            } else {
                break;
            }
        }
        let (table, _) = read_qualified_table_ident(&sql[after_kw..])?;
        return (!table.is_empty()).then_some(table);
    }
    None
}

/// Returns `Some(normalized_table)` when a Phase 1 `Decision::Pre` statement
/// operates on a single table without cross-table dependencies, so multiple
/// worker connections can execute independent table DDL in parallel while
/// keeping all statements for the same table in order on the same worker.
fn pre_target_table(driver: Driver, sql: &str) -> Option<String> {
    if is_tx_begin(sql) || is_lock_or_unlock_tables(sql) {
        return None;
    }
    let class = resume::classify(driver, sql);
    if class.session || !matches!(class.tx, Tx::None | Tx::ImplicitCommit) {
        return None;
    }
    let mut words = Words::new(sql);
    let first = words.next()?;
    if first.eq_ignore_ascii_case("DROP") {
        let second = words.next()?;
        if !second.eq_ignore_ascii_case("TABLE") {
            return None;
        }
        let mut after_kw = words.index;
        if let Some(w1) = words.next() {
            if w1.eq_ignore_ascii_case("IF") {
                if let Some(w2) = words.next() {
                    if w2.eq_ignore_ascii_case("EXISTS") {
                        after_kw = words.index;
                    }
                }
            }
        }
        let (table, consumed) = read_qualified_table_ident(&sql[after_kw..])?;
        let rest = &sql[after_kw + consumed..];
        if has_top_level_comma(rest) {
            return None;
        }
        if driver == Driver::Postgres && Words::new(rest).any(|w| w.eq_ignore_ascii_case("CASCADE"))
        {
            return None;
        }
        return Some(table);
    }
    if first.eq_ignore_ascii_case("TRUNCATE") {
        let mut after_kw = words.index;
        while let Some(w) = words.next() {
            if w.eq_ignore_ascii_case("TABLE") || w.eq_ignore_ascii_case("ONLY") {
                after_kw = words.index;
            } else {
                break;
            }
        }
        let (table, consumed) = read_qualified_table_ident(&sql[after_kw..])?;
        let rest = &sql[after_kw + consumed..];
        if has_top_level_comma(rest) {
            return None;
        }
        if driver == Driver::Postgres && Words::new(rest).any(|w| w.eq_ignore_ascii_case("CASCADE"))
        {
            return None;
        }
        return Some(table);
    }
    if first.eq_ignore_ascii_case("CREATE") {
        let mut word = words.next()?;
        if word.eq_ignore_ascii_case("UNLOGGED") {
            word = words.next()?;
        }
        if !word.eq_ignore_ascii_case("TABLE") {
            return None;
        }
        let mut after_kw = words.index;
        if let Some(w1) = words.next() {
            if w1.eq_ignore_ascii_case("IF") {
                if let Some(w2) = words.next() {
                    if w2.eq_ignore_ascii_case("NOT") {
                        if let Some(w3) = words.next() {
                            if w3.eq_ignore_ascii_case("EXISTS") {
                                after_kw = words.index;
                            }
                        }
                    }
                }
            }
        }
        let (table, consumed) = read_qualified_table_ident(&sql[after_kw..])?;
        let rest = &sql[after_kw + consumed..];
        for w in Words::new(rest) {
            if w.eq_ignore_ascii_case("LIKE")
                || w.eq_ignore_ascii_case("INHERITS")
                || w.eq_ignore_ascii_case("PARTITION")
                || w.eq_ignore_ascii_case("SELECT")
                || w.eq_ignore_ascii_case("REFERENCES")
                || w.eq_ignore_ascii_case("FOREIGN")
            {
                return None;
            }
        }
        return Some(table);
    }
    if first.eq_ignore_ascii_case("ALTER") {
        let second = words.next()?;
        if !second.eq_ignore_ascii_case("TABLE") {
            return None;
        }
        let mut after_kw = words.index;
        while let Some(w) = words.next() {
            if w.eq_ignore_ascii_case("IF")
                || w.eq_ignore_ascii_case("EXISTS")
                || w.eq_ignore_ascii_case("ONLY")
            {
                after_kw = words.index;
            } else {
                break;
            }
        }
        let (table, consumed) = read_qualified_table_ident(&sql[after_kw..])?;
        let rest = &sql[after_kw + consumed..];
        for w in Words::new(rest) {
            if w.eq_ignore_ascii_case("RENAME")
                || w.eq_ignore_ascii_case("REFERENCES")
                || w.eq_ignore_ascii_case("FOREIGN")
                || w.eq_ignore_ascii_case("INHERIT")
                || w.eq_ignore_ascii_case("ATTACH")
                || w.eq_ignore_ascii_case("DETACH")
            {
                return None;
            }
        }
        return Some(table);
    }
    if first.eq_ignore_ascii_case("COMMENT") {
        if !words.next().is_some_and(|w| w.eq_ignore_ascii_case("ON")) {
            return None;
        }
        if !words
            .next()
            .is_some_and(|w| w.eq_ignore_ascii_case("TABLE"))
        {
            return None;
        }
        let after_kw = words.index;
        let (table, _) = read_qualified_table_ident(&sql[after_kw..])?;
        return Some(table);
    }
    None
}

#[derive(Clone)]
enum ChunkSource {
    Memory(Arc<[u8]>),
    Disk(PathBuf),
}

#[derive(Clone)]
struct DataChunk {
    table_id: usize,
    source: ChunkSource,
    lines: u64,
    statements: u64,
    bytes: u64,
}

enum ChunkWriter {
    Memory(Vec<u8>),
    Disk {
        path: PathBuf,
        writer: Option<BufWriter<File>>,
    },
}

struct ChunkFile {
    writer: ChunkWriter,
}

impl ChunkFile {
    fn new() -> Self {
        let mut buf = Vec::with_capacity(4096);
        buf.extend_from_slice(MAGIC);
        Self {
            writer: ChunkWriter::Memory(buf),
        }
    }

    fn reserve(&mut self, bytes: usize, temps: &mut TempFiles) -> Result<(), String> {
        let ChunkWriter::Memory(buf) = &mut self.writer else {
            return Ok(());
        };
        if temps.memory_bytes + (bytes as u64) <= CHUNK_TARGET {
            temps.memory_bytes += bytes as u64;
            return Ok(());
        }
        let path = std::env::temp_dir().join(format!("recon-sql-load-{}", uuid::Uuid::new_v4()));
        let file = File::create(&path)
            .map_err(|err| format!("Could not create a temporary import file: {err}"))?;
        temps.paths.push(path.clone());
        let mut writer = BufWriter::with_capacity(CHUNK_IO_BUFFER, file);
        write_all(&mut writer, buf)?;
        let buffered = buf.len().saturating_sub(MAGIC.len()) as u64;
        temps.memory_bytes = temps.memory_bytes.saturating_sub(buffered);
        self.writer = ChunkWriter::Disk {
            path,
            writer: Some(writer),
        };
        Ok(())
    }

    fn suspend_disk(&mut self) -> Result<(), String> {
        if let ChunkWriter::Disk { writer, .. } = &mut self.writer {
            if let Some(mut w) = writer.take() {
                w.flush()
                    .map_err(|err| format!("Could not write the import chunk: {err}"))?;
            }
        }
        Ok(())
    }

    fn finish(self, lines: u64, statements: u64, bytes: u64) -> Result<DataChunk, String> {
        let source = match self.writer {
            ChunkWriter::Memory(buf) => ChunkSource::Memory(Arc::from(buf)),
            ChunkWriter::Disk { path, writer } => {
                if let Some(mut writer) = writer {
                    writer
                        .flush()
                        .map_err(|err| format!("Could not write the import chunk: {err}"))?;
                }
                ChunkSource::Disk(path)
            }
        };
        Ok(DataChunk {
            table_id: 0,
            source,
            lines,
            statements,
            bytes,
        })
    }
}

impl Write for ChunkFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match &mut self.writer {
            ChunkWriter::Memory(vec) => {
                vec.extend_from_slice(buf);
                Ok(buf.len())
            }
            ChunkWriter::Disk { path, writer } => {
                if writer.is_none() {
                    let file = std::fs::OpenOptions::new().append(true).open(&*path)?;
                    *writer = Some(BufWriter::with_capacity(CHUNK_IO_BUFFER, file));
                }
                writer.as_mut().expect("disk chunk writer").write(buf)
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut self.writer {
            ChunkWriter::Memory(_) | ChunkWriter::Disk { writer: None, .. } => Ok(()),
            ChunkWriter::Disk {
                writer: Some(writer),
                ..
            } => writer.flush(),
        }
    }
}

fn finish_bins(bins: Vec<Bin>, workers: usize) -> Result<(Vec<DataChunk>, Vec<u64>), String> {
    let mut chunks = Vec::new();
    for bin in bins {
        if let Some(file) = bin.file {
            let mut chunk = file.finish(bin.lines, bin.statements, bin.bytes)?;
            chunk.table_id = bin.table_id;
            chunks.push(chunk);
        }
    }
    let groups = assign_workers(&chunks, workers);
    let counts = groups
        .iter()
        .map(|group| group.iter().map(|chunk| chunk.statements).sum())
        .collect();
    Ok((chunks, counts))
}

#[derive(Default)]
struct TempFiles {
    paths: Vec<PathBuf>,
    memory_bytes: u64,
}

impl Drop for TempFiles {
    fn drop(&mut self) {
        for path in &self.paths {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn write_sql_record(
    writer: &mut impl Write,
    index: usize,
    line: usize,
    sql: &str,
) -> Result<(), String> {
    write_all(writer, &[TAG_SQL])?;
    write_u64(writer, index as u64)?;
    write_u64(writer, line as u64)?;
    write_bytes(writer, sql.as_bytes())
}

fn write_copy_start(
    writer: &mut impl Write,
    index: usize,
    line: usize,
    sql: &str,
) -> Result<(), String> {
    write_all(writer, &[TAG_COPY])?;
    write_u64(writer, index as u64)?;
    write_u64(writer, line as u64)?;
    write_bytes(writer, sql.as_bytes())
}

fn write_copy_data(writer: &mut impl Write, line: &[u8]) -> Result<(), String> {
    write_all(writer, &[TAG_DATA])?;
    write_bytes(writer, line)
}

fn write_copy_end(writer: &mut impl Write, text_bytes: u64) -> Result<(), String> {
    write_all(writer, &[TAG_END])?;
    write_u64(writer, text_bytes)
}

fn write_all(writer: &mut impl Write, bytes: &[u8]) -> Result<(), String> {
    writer
        .write_all(bytes)
        .map_err(|err| format!("Could not write the import chunk: {err}"))
}

fn write_u64(writer: &mut impl Write, value: u64) -> Result<(), String> {
    write_all(writer, &value.to_le_bytes())
}

fn write_bytes(writer: &mut impl Write, bytes: &[u8]) -> Result<(), String> {
    write_u64(writer, bytes.len() as u64)?;
    write_all(writer, bytes)
}

fn read_magic(reader: &mut impl Read) -> Result<(), String> {
    let mut magic = [0u8; 5];
    read_exact(reader, &mut magic)?;
    if magic != MAGIC {
        return Err("The import chunk is not a Recon SQL chunk.".into());
    }
    Ok(())
}

fn read_tag(reader: &mut impl Read) -> Result<Option<u8>, String> {
    let mut byte = [0u8; 1];
    match reader
        .read(&mut byte)
        .map_err(|err| format!("Could not read the import chunk: {err}"))?
    {
        0 => Ok(None),
        _ => Ok(Some(byte[0])),
    }
}

fn read_exact(reader: &mut impl Read, buf: &mut [u8]) -> Result<(), String> {
    reader.read_exact(buf).map_err(|err| {
        if err.kind() == io::ErrorKind::UnexpectedEof {
            "The import chunk ended early.".into()
        } else {
            format!("Could not read the import chunk: {err}")
        }
    })
}

fn read_u64(reader: &mut impl Read) -> Result<u64, String> {
    let mut buf = [0u8; 8];
    read_exact(reader, &mut buf)?;
    Ok(u64::from_le_bytes(buf))
}

fn read_usize(reader: &mut impl Read) -> Result<usize, String> {
    let value = read_u64(reader)?;
    usize::try_from(value).map_err(|_| "The import chunk is too large.".to_string())
}

fn read_vec(reader: &mut impl Read) -> Result<Vec<u8>, String> {
    let len = usize::try_from(read_u64(reader)?)
        .map_err(|_| "The import chunk is too large.".to_string())?;
    let mut buf = vec![0u8; len];
    if len > 0 {
        read_exact(reader, &mut buf)?;
    }
    Ok(buf)
}

fn read_sql(reader: &mut impl Read) -> Result<String, String> {
    let bytes = read_vec(reader)?;
    String::from_utf8(bytes).map_err(|_| "The import chunk is not valid UTF-8.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[derive(Debug, PartialEq, Eq)]
    struct Classified {
        phase: Phase,
        sql: String,
        copy_tail: Option<String>,
    }

    fn classify(driver: Driver, script: &str, workers: usize) -> Vec<Classified> {
        classify_result(driver, script, workers).unwrap_or_else(|err| panic!("{err}"))
    }

    fn classify_result(
        driver: Driver,
        script: &str,
        workers: usize,
    ) -> Result<Vec<Classified>, String> {
        let mut stream = Stream::new(Cursor::new(script.as_bytes()), driver);
        let cancel = AtomicBool::new(false);
        let mut seen_data = false;
        let mut items = Vec::new();
        while let Some(stmt) = stream.next_statement(&cancel)? {
            if is_copy_from_stdin(&stmt.sql) {
                let mut tail = String::new();
                let (_, _, terminator) = stream.consume_copy(&cancel, |line| {
                    tail.push_str(&String::from_utf8_lossy(line));
                    Ok(())
                })?;
                tail.push_str(&String::from_utf8_lossy(&terminator));
                seen_data = true;
                items.push(Classified {
                    phase: Phase::Data,
                    sql: stmt.sql,
                    copy_tail: Some(tail),
                });
                continue;
            }
            let decision = decide_phase(&stmt.sql, seen_data, workers);
            if decision == Decision::Data {
                seen_data = true;
            }
            if let Some(phase) = decision.phase() {
                items.push(Classified {
                    phase,
                    sql: stmt.sql,
                    copy_tail: None,
                });
            }
        }
        Ok(items)
    }

    fn phases(items: &[Classified]) -> Vec<Phase> {
        items.iter().map(|item| item.phase).collect()
    }

    #[test]
    fn recon_like_mysql_postgres_and_sqlite_sequences() {
        let mysql = classify(
            Driver::Mysql,
            r#"
SET NAMES utf8mb4;
DROP TABLE IF EXISTS users;
CREATE TABLE users (
  id int NOT NULL,
  PRIMARY KEY (id)
);
DELIMITER $$
CREATE PROCEDURE keep_seed()
BEGIN
  INSERT INTO users (id) VALUES (2);
END$$
DELIMITER ;
INSERT INTO users (id) VALUES (1);
CREATE INDEX idx_users ON users (id);
CREATE TRIGGER users_bi BEFORE INSERT ON users FOR EACH ROW SET NEW.id = NEW.id;
ALTER TABLE users ADD CONSTRAINT fk_users FOREIGN KEY (id) REFERENCES users (id);
SET FOREIGN_KEY_CHECKS=@OLD_FOREIGN_KEY_CHECKS;
COMMIT;
"#,
            1,
        );
        assert!(mysql.iter().any(|item| item.phase == Phase::Pre
            && item.sql.to_ascii_uppercase().contains("CREATE PROCEDURE")
            && item.sql.contains("VALUES (2)")));
        let mysql_data: Vec<_> = mysql
            .iter()
            .filter(|item| item.phase == Phase::Data)
            .collect();
        assert_eq!(mysql_data.len(), 1);
        assert!(mysql_data[0].sql.contains("VALUES (1)"));
        assert_eq!(
            mysql
                .iter()
                .find(|item| item.sql.to_ascii_uppercase().contains("CREATE INDEX"))
                .map(|item| item.phase),
            Some(Phase::Post)
        );
        assert_eq!(
            mysql
                .iter()
                .find(|item| item.sql.to_ascii_uppercase().contains("CREATE TRIGGER"))
                .map(|item| item.phase),
            Some(Phase::Post)
        );
        assert_eq!(
            mysql
                .iter()
                .find(|item| item.sql.to_ascii_uppercase().contains("ADD CONSTRAINT"))
                .map(|item| item.phase),
            Some(Phase::Post)
        );
        assert_eq!(mysql.first().map(|item| item.phase), Some(Phase::Pre));
        assert!(mysql
            .first()
            .unwrap()
            .sql
            .to_ascii_uppercase()
            .contains("SET"));
        assert_eq!(
            mysql
                .iter()
                .rev()
                .find(|item| item.sql.to_ascii_uppercase().starts_with("SET"))
                .map(|item| item.phase),
            Some(Phase::Post)
        );
        assert_eq!(mysql.last().map(|item| item.phase), Some(Phase::Post));

        let postgres = classify(
            Driver::Postgres,
            r#"
SET client_encoding = 'UTF8';
CREATE TABLE users (
  id integer GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
  name text
);
COMMENT ON TABLE users IS 'People';
CREATE FUNCTION users_bi() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
  NEW.name := upper(NEW.name);
  RETURN NEW;
END
$$;
INSERT INTO users (name) VALUES ('ada');
CREATE VIEW active_users AS SELECT id, name FROM users;
COMMENT ON VIEW active_users IS 'Active';
CREATE UNIQUE INDEX idx_users_name ON users (name);
CREATE TRIGGER users_bi
BEFORE INSERT ON users
FOR EACH ROW
EXECUTE FUNCTION users_bi();
ALTER TABLE users
  ADD CONSTRAINT users_name_key UNIQUE (name);
SELECT setval('users_id_seq', 1, true);
SELECT pg_catalog.setval('public.users_id_seq', 1, true);
COMMIT;
"#,
            4,
        );
        assert_eq!(
            postgres
                .iter()
                .find(|item| item.sql.to_ascii_uppercase().contains("CREATE FUNCTION"))
                .map(|item| item.phase),
            Some(Phase::Pre)
        );
        assert!(postgres.iter().any(|item| {
            item.phase == Phase::Pre && item.sql.to_ascii_uppercase().contains("CREATE TABLE")
        }));
        assert_eq!(
            postgres
                .iter()
                .find(|item| item.sql.to_ascii_uppercase().contains("COMMENT ON TABLE"))
                .map(|item| item.phase),
            Some(Phase::Pre)
        );
        assert_eq!(
            postgres
                .iter()
                .find(|item| item.sql.to_ascii_uppercase().contains("COMMENT ON VIEW"))
                .map(|item| item.phase),
            Some(Phase::Post)
        );
        assert_eq!(
            postgres
                .iter()
                .find(|item| item.phase == Phase::Data)
                .map(|item| item.sql.contains("ada")),
            Some(true)
        );
        assert_eq!(
            postgres
                .iter()
                .find(|item| item
                    .sql
                    .to_ascii_uppercase()
                    .contains("CREATE UNIQUE INDEX"))
                .map(|item| item.phase),
            Some(Phase::Post)
        );
        assert_eq!(
            postgres
                .iter()
                .find(|item| item.sql.to_ascii_uppercase().contains("CREATE TRIGGER"))
                .map(|item| item.phase),
            Some(Phase::Post)
        );
        assert_eq!(
            postgres
                .iter()
                .find(|item| item.sql.to_ascii_uppercase().contains("ADD CONSTRAINT"))
                .map(|item| item.phase),
            Some(Phase::Post)
        );
        let setvals: Vec<_> = postgres
            .iter()
            .filter(|item| item.sql.to_ascii_lowercase().contains("setval"))
            .map(|item| item.phase)
            .collect();
        assert_eq!(setvals, vec![Phase::Post, Phase::Post]);
        assert_eq!(postgres.first().map(|item| item.phase), Some(Phase::Pre));
        assert_eq!(postgres.last().map(|item| item.phase), Some(Phase::Post));

        let sqlite = classify(
            Driver::Sqlite,
            r#"
PRAGMA foreign_keys=OFF;
BEGIN TRANSACTION;
CREATE TABLE users (
  id integer primary key
);
INSERT INTO users (id) VALUES (1);
CREATE INDEX idx_users ON users (id);
CREATE TRIGGER users_bi
BEFORE INSERT ON users
FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'blocked');
END;
ALTER TABLE users ADD CONSTRAINT fk_users FOREIGN KEY (id) REFERENCES users (id);
COMMIT;
"#,
            1,
        );
        assert_eq!(
            phases(&sqlite),
            vec![
                Phase::Pre,
                Phase::Pre,
                Phase::Pre,
                Phase::Data,
                Phase::Post,
                Phase::Post,
                Phase::Post,
                Phase::Post,
            ]
        );
        let trigger = sqlite
            .iter()
            .find(|item| item.sql.to_ascii_uppercase().contains("CREATE TRIGGER"))
            .unwrap();
        assert_eq!(trigger.phase, Phase::Post);
        assert!(trigger.sql.to_ascii_uppercase().contains("RAISE"));
        assert!(sqlite
            .first()
            .unwrap()
            .sql
            .to_ascii_uppercase()
            .contains("PRAGMA"));
        assert!(sqlite[1].sql.to_ascii_uppercase().contains("BEGIN"));
        assert!(sqlite
            .last()
            .unwrap()
            .sql
            .to_ascii_uppercase()
            .contains("COMMIT"));
    }

    #[test]
    fn mysqldump_lock_and_footer_sets() {
        let script = r#"
/*!40101 SET @OLD_CHARACTER_SET_CLIENT=@@CHARACTER_SET_CLIENT */;
/*!40101 SET @OLD_FOREIGN_KEY_CHECKS=@@FOREIGN_KEY_CHECKS, FOREIGN_KEY_CHECKS=0 */;
SET NAMES utf8mb4;
DROP TABLE IF EXISTS `users`;
CREATE TABLE `users` (
  `id` int NOT NULL
);
LOCK TABLES `users` WRITE;
/*!40000 ALTER TABLE `users` DISABLE KEYS */;
INSERT INTO `users` VALUES (1);
/*!40000 ALTER TABLE `users` ENABLE KEYS */;
UNLOCK TABLES;
/*!40101 SET FOREIGN_KEY_CHECKS=@OLD_FOREIGN_KEY_CHECKS */;
/*!40101 SET CHARACTER_SET_CLIENT=@OLD_CHARACTER_SET_CLIENT */;
"#;
        let single = classify(Driver::Mysql, script, 1);
        assert!(single.iter().any(|item| item.phase == Phase::Pre
            && item.sql.to_ascii_uppercase().contains("LOCK TABLES")));
        assert!(single.iter().any(|item| item.phase == Phase::Pre
            && item.sql.to_ascii_uppercase().contains("DISABLE KEYS")));
        assert!(single.iter().any(|item| item.phase == Phase::Data
            && item.sql.to_ascii_uppercase().contains("INSERT INTO")));
        assert!(single.iter().any(|item| item.phase == Phase::Post
            && item.sql.to_ascii_uppercase().contains("ENABLE KEYS")));
        assert!(single.iter().any(|item| item.phase == Phase::Post
            && item.sql.to_ascii_uppercase().contains("UNLOCK TABLES")));
        let sets: Vec<_> = single
            .iter()
            .filter(|item| {
                Words::new(&item.sql)
                    .next()
                    .is_some_and(|w| w.eq_ignore_ascii_case("SET"))
            })
            .map(|item| item.phase)
            .collect();
        assert!(sets.len() >= 4, "{sets:?}");
        assert!(sets.contains(&Phase::Pre));
        assert!(sets.contains(&Phase::Post));
        let first_data = single
            .iter()
            .position(|item| item.phase == Phase::Data)
            .unwrap();
        assert!(single[..first_data]
            .iter()
            .filter(|item| {
                Words::new(&item.sql)
                    .next()
                    .is_some_and(|w| w.eq_ignore_ascii_case("SET"))
            })
            .all(|item| item.phase == Phase::Pre));
        assert!(single[first_data + 1..]
            .iter()
            .filter(|item| {
                Words::new(&item.sql)
                    .next()
                    .is_some_and(|w| w.eq_ignore_ascii_case("SET"))
            })
            .all(|item| item.phase == Phase::Post));

        let parallel = classify(Driver::Mysql, script, 8);
        assert!(parallel.iter().all(|item| {
            let upper = item.sql.to_ascii_uppercase();
            !upper.contains("LOCK TABLES") && !upper.contains("UNLOCK TABLES")
        }));
        assert!(parallel.iter().any(|item| item.phase == Phase::Pre
            && item.sql.to_ascii_uppercase().contains("DISABLE KEYS")));
        assert!(parallel.iter().any(|item| item.phase == Phase::Post
            && item.sql.to_ascii_uppercase().contains("ENABLE KEYS")));
        assert!(parallel.iter().any(|item| item.phase == Phase::Post
            && Words::new(&item.sql)
                .next()
                .is_some_and(|w| w.eq_ignore_ascii_case("SET"))));
    }

    #[test]
    fn copy_from_stdin_stays_in_phase_two() {
        let items = classify(
            Driver::Postgres,
            "SET search_path = public;\n\
             CREATE TABLE users (id int, name text);\n\
             COPY public.users (id, name) FROM stdin;\n\
             1\talice\n\
             INSERT INTO fake VALUES (9);\n\
             2\tbob\n\
             \\.\n\
             CREATE INDEX users_id_idx ON users (id);\n\
             SELECT pg_catalog.setval('users_id_seq', 2, true);\n\
             SET search_path = public;\n",
            4,
        );
        let data: Vec<_> = items
            .iter()
            .filter(|item| item.phase == Phase::Data)
            .collect();
        assert_eq!(data.len(), 1);
        assert!(is_copy_from_stdin(&data[0].sql));
        assert!(!data[0].sql.contains("alice"));
        let tail = data[0].copy_tail.as_deref().unwrap();
        assert!(tail.contains("1\talice\n"), "{tail:?}");
        assert!(tail.contains("INSERT INTO fake VALUES (9);\n"), "{tail:?}");
        assert!(tail.contains("2\tbob\n"), "{tail:?}");
        assert!(tail.contains("\\.\n"), "{tail:?}");
        assert!(items.iter().all(|item| !item.sql.contains("alice")));
        assert!(items
            .iter()
            .all(|item| !item.sql.contains("INSERT INTO fake")));
        assert_eq!(items.first().map(|item| item.phase), Some(Phase::Pre));
        assert!(items
            .first()
            .unwrap()
            .sql
            .to_ascii_uppercase()
            .contains("SET"));
        assert_eq!(
            items
                .iter()
                .find(|item| item.sql.to_ascii_uppercase().contains("CREATE INDEX"))
                .map(|item| item.phase),
            Some(Phase::Post)
        );
        assert_eq!(
            items
                .iter()
                .find(|item| item.sql.to_ascii_lowercase().contains("setval"))
                .map(|item| item.phase),
            Some(Phase::Post)
        );
        assert_eq!(items.last().map(|item| item.phase), Some(Phase::Post));
        assert!(items
            .last()
            .unwrap()
            .sql
            .to_ascii_uppercase()
            .contains("SET"));
    }

    #[test]
    fn data_chunks_fill_the_smallest_bin() {
        let mut bins = Vec::new();
        let first = select_bin(&mut bins, 2, 10);
        assert_eq!(first, 0);
        bins[first].bytes += 4;
        let second = select_bin(&mut bins, 2, 10);
        assert_eq!(second, 1);
        bins[second].bytes += 4;
        let third = select_bin(&mut bins, 2, 10);
        assert_eq!(third, 0);
        bins[third].bytes += 6;
        let fourth = select_bin(&mut bins, 2, 10);
        assert_eq!(fourth, 1);
        bins[fourth].bytes += 6;
        let spilled = select_bin(&mut bins, 2, 10);
        assert_eq!(spilled, 2);
        assert_eq!(bins.len(), 3);
        assert_eq!(bins[2].bytes, 0);
    }

    #[test]
    fn copy_chunk_round_trip_keeps_the_payload_together() {
        let mut bytes = Vec::new();
        {
            let mut writer = Cursor::new(&mut bytes);
            write_all(&mut writer, MAGIC).unwrap();
            write_sql_record(&mut writer, 0, 2, "CREATE TABLE t (id int)").unwrap();
            write_copy_start(&mut writer, 1, 4, "COPY t FROM stdin").unwrap();
            write_copy_data(&mut writer, b"1\talice\n").unwrap();
            write_copy_data(&mut writer, b"2\tbob\n").unwrap();
            write_copy_end(&mut writer, 40).unwrap();
            write_sql_record(&mut writer, 2, 8, "CREATE INDEX t_id ON t (id)").unwrap();
        }
        let mut reader = Cursor::new(bytes);
        read_magic(&mut reader).unwrap();
        assert_eq!(read_tag(&mut reader).unwrap(), Some(TAG_SQL));
        assert_eq!(read_usize(&mut reader).unwrap(), 0);
        assert_eq!(read_usize(&mut reader).unwrap(), 2);
        assert_eq!(read_sql(&mut reader).unwrap(), "CREATE TABLE t (id int)");
        assert_eq!(read_tag(&mut reader).unwrap(), Some(TAG_COPY));
        assert_eq!(read_usize(&mut reader).unwrap(), 1);
        assert_eq!(read_usize(&mut reader).unwrap(), 4);
        assert_eq!(read_sql(&mut reader).unwrap(), "COPY t FROM stdin");
        assert_eq!(read_tag(&mut reader).unwrap(), Some(TAG_DATA));
        assert_eq!(read_vec(&mut reader).unwrap(), b"1\talice\n");
        assert_eq!(read_tag(&mut reader).unwrap(), Some(TAG_DATA));
        assert_eq!(read_vec(&mut reader).unwrap(), b"2\tbob\n");
        assert_eq!(read_tag(&mut reader).unwrap(), Some(TAG_END));
        assert_eq!(read_u64(&mut reader).unwrap(), 40);
        assert_eq!(read_tag(&mut reader).unwrap(), Some(TAG_SQL));
        assert_eq!(read_usize(&mut reader).unwrap(), 2);
        assert_eq!(read_usize(&mut reader).unwrap(), 8);
        assert_eq!(
            read_sql(&mut reader).unwrap(),
            "CREATE INDEX t_id ON t (id)"
        );
        assert_eq!(read_tag(&mut reader).unwrap(), None);
    }

    #[test]
    fn lost_connection_keeps_the_in_flight_index() {
        let message = format!("{} (broken pipe).", crate::db::CONNECTION_LOST);
        let err = statement_failure(7, 12, message.clone());
        match err {
            LoadSqlError::LostConnection(lost) => {
                assert_eq!(lost.in_flight, vec![7]);
                assert_eq!(lost.source, message);
            }
            LoadSqlError::Failed(message) => panic!("collapsed into {message}"),
        }
        assert_eq!(
            statement_failure(7, 12, "syntax error".into()),
            LoadSqlError::Failed("Line 12: syntax error".into())
        );
    }

    fn read_chunk_statements(chunk: &DataChunk) -> Vec<(usize, String)> {
        let mut reader: Box<dyn Read> = match &chunk.source {
            ChunkSource::Memory(bytes) => Box::new(Cursor::new(bytes.as_ref())),
            ChunkSource::Disk(path) => Box::new(BufReader::new(File::open(path).unwrap())),
        };
        read_magic(&mut reader).unwrap();
        let mut out = Vec::new();
        loop {
            match read_tag(&mut reader).unwrap() {
                None => break,
                Some(TAG_SQL) => {
                    let index = read_usize(&mut reader).unwrap();
                    let _line = read_usize(&mut reader).unwrap();
                    out.push((index, read_sql(&mut reader).unwrap()));
                }
                Some(TAG_COPY) => {
                    let index = read_usize(&mut reader).unwrap();
                    let _line = read_usize(&mut reader).unwrap();
                    let sql = read_sql(&mut reader).unwrap();
                    drain_copy(&mut reader).unwrap();
                    out.push((index, sql));
                }
                Some(tag) => panic!("unexpected chunk tag {tag}"),
            }
        }
        out
    }

    /// Phase 1's split of a file, routed the way the loader routes it, without running anything.
    struct Spooled {
        pre: Vec<(usize, String)>,
        data: Vec<DataChunk>,
        data_statements: Vec<u64>,
        post: Vec<(usize, String)>,
    }

    fn spool_dump<R: BufRead>(
        reader: R,
        driver: Driver,
        workers: usize,
        split_tables: bool,
        cancel: &AtomicBool,
        temps: &mut TempFiles,
    ) -> Result<Spooled, String> {
        let mut stream = Stream::new(reader, driver);
        let mut spool = Spool::new(workers, split_tables);
        let mut pre = Vec::new();
        let mut post = Vec::new();
        let mut seen_data = false;
        let mut next_index = 0;
        while let Some(stmt) = stream.next_statement(cancel)? {
            let index = next_index;
            next_index += 1;
            if driver == Driver::Postgres && is_copy_from_stdin(&stmt.sql) {
                spool
                    .push_copy(index, &stmt, &mut stream, cancel, temps)
                    .map_err(|err| err.to_string())?;
                seen_data = true;
                continue;
            }
            let (decision, table) = decide_phase_with_table(&stmt.sql, seen_data, workers);
            match decision {
                Decision::Skip => {}
                Decision::Data => {
                    spool.push_sql(index, &stmt, table, temps)?;
                    seen_data = true;
                }
                Decision::Pre => pre.push((index, stmt.sql)),
                Decision::Post => post.push((index, stmt.sql)),
            }
        }
        let (data, data_statements) = spool.finish()?;
        Ok(Spooled {
            pre,
            data,
            data_statements,
            post,
        })
    }

    fn spool(driver: Driver, script: &str, workers: usize, split_tables: bool) -> Spooled {
        let mut temps = TempFiles::default();
        spool_dump(
            Cursor::new(script.as_bytes()),
            driver,
            workers,
            split_tables,
            &AtomicBool::new(false),
            &mut temps,
        )
        .unwrap()
    }

    fn planned_statements(driver: Driver, script: &str, workers: usize) -> Vec<(usize, String)> {
        let plan = spool(driver, script, workers, false);
        let mut all: Vec<(usize, String)> = plan.pre.into_iter().chain(plan.post).collect();
        for chunk in &plan.data {
            all.extend(read_chunk_statements(chunk));
        }
        all.sort_by_key(|(index, _)| *index);
        all
    }

    fn statement_index(items: &[(usize, String)], needle: &str) -> usize {
        items
            .iter()
            .find(|(_, sql)| sql.contains(needle))
            .unwrap_or_else(|| panic!("missing {needle}"))
            .0
    }

    #[test]
    fn file_order_indexes_keep_skipped_lock_tables() {
        let script = r#"
/*!40101 SET @OLD_FOREIGN_KEY_CHECKS=@@FOREIGN_KEY_CHECKS, FOREIGN_KEY_CHECKS=0 */;
DROP TABLE IF EXISTS `users`;
CREATE TABLE `users` (
  `id` int NOT NULL
);
LOCK TABLES `users` WRITE;
INSERT INTO `users` VALUES (1);
UNLOCK TABLES;
/*!40101 SET FOREIGN_KEY_CHECKS=@OLD_FOREIGN_KEY_CHECKS */;
"#;
        let single = planned_statements(Driver::Mysql, script, 1);
        let parallel = planned_statements(Driver::Mysql, script, 8);
        let single_indexes: Vec<_> = single.iter().map(|(index, _)| *index).collect();
        assert_eq!(
            single_indexes,
            (0..single_indexes.len()).collect::<Vec<_>>()
        );
        let lock_index = statement_index(&single, "LOCK TABLES");
        let unlock_index = statement_index(&single, "UNLOCK TABLES");
        let insert_index = statement_index(&single, "INSERT INTO");
        let footer_index = statement_index(&single, "FOREIGN_KEY_CHECKS=@OLD_FOREIGN_KEY_CHECKS");
        assert!(lock_index < insert_index && insert_index < unlock_index);
        assert!(unlock_index < footer_index);
        assert_eq!(statement_index(&parallel, "INSERT INTO"), insert_index);
        assert_eq!(
            statement_index(&parallel, "FOREIGN_KEY_CHECKS=@OLD_FOREIGN_KEY_CHECKS"),
            footer_index
        );
        assert!(parallel.iter().all(|(index, sql)| {
            let upper = sql.to_ascii_uppercase();
            *index != lock_index
                && *index != unlock_index
                && !upper.contains("LOCK TABLES")
                && !upper.contains("UNLOCK TABLES")
        }));
    }

    #[test]
    fn drain_copy_leaves_the_next_statement_aligned() {
        let mut bytes = Vec::new();
        {
            let mut writer = Cursor::new(&mut bytes);
            write_all(&mut writer, MAGIC).unwrap();
            write_copy_start(&mut writer, 3, 4, "COPY t FROM stdin").unwrap();
            write_copy_data(&mut writer, b"1\talice\n").unwrap();
            write_copy_end(&mut writer, 19).unwrap();
            write_sql_record(&mut writer, 4, 9, "SELECT 1").unwrap();
        }
        let mut reader = Cursor::new(bytes);
        read_magic(&mut reader).unwrap();
        assert_eq!(read_tag(&mut reader).unwrap(), Some(TAG_COPY));
        assert_eq!(read_usize(&mut reader).unwrap(), 3);
        assert_eq!(read_usize(&mut reader).unwrap(), 4);
        assert_eq!(read_sql(&mut reader).unwrap(), "COPY t FROM stdin");
        assert_eq!(drain_copy(&mut reader).unwrap(), 19);
        assert_eq!(read_tag(&mut reader).unwrap(), Some(TAG_SQL));
        assert_eq!(read_usize(&mut reader).unwrap(), 4);
        assert_eq!(read_usize(&mut reader).unwrap(), 9);
        assert_eq!(read_sql(&mut reader).unwrap(), "SELECT 1");
    }

    struct CancelAfterLinesReader {
        lines: Vec<String>,
        index: usize,
        cancel_at: usize,
        cancel: Arc<AtomicBool>,
    }

    impl Read for CancelAfterLinesReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.index >= self.lines.len() {
                return Ok(0);
            }
            if self.index == self.cancel_at {
                self.cancel.store(true, Ordering::Relaxed);
            }
            let bytes = self.lines[self.index].as_bytes();
            let len = bytes.len().min(buf.len());
            buf[..len].copy_from_slice(&bytes[..len]);
            if len == bytes.len() {
                self.index += 1;
            } else {
                self.lines[self.index].drain(..len);
            }
            Ok(len)
        }
    }

    #[test]
    fn cancelling_during_planning_stops_inside_multiline_statements_and_copy_data() {
        let cancel = Arc::new(AtomicBool::new(false));
        let reader = BufReader::new(CancelAfterLinesReader {
            lines: vec![
                "INSERT INTO t VALUES\n".into(),
                "  (1),\n".into(),
                "  (2),\n".into(),
                "  (3);\n".into(),
            ],
            index: 0,
            cancel_at: 1,
            cancel: cancel.clone(),
        });
        let mut temps = TempFiles::default();
        let err = spool_dump(reader, Driver::Mysql, 1, false, &cancel, &mut temps)
            .err()
            .expect("cancelled during multiline statement");
        assert!(err.contains("Import cancelled."), "{err}");

        let cancel = Arc::new(AtomicBool::new(false));
        let reader = BufReader::new(CancelAfterLinesReader {
            lines: vec![
                "COPY public.t (id) FROM stdin;\n".into(),
                "1\n".into(),
                "2\n".into(),
                "3\n".into(),
                "\\.\n".into(),
            ],
            index: 0,
            cancel_at: 2,
            cancel: cancel.clone(),
        });
        let mut temps = TempFiles::default();
        let err = spool_dump(reader, Driver::Postgres, 1, false, &cancel, &mut temps)
            .err()
            .expect("cancelled during COPY data");
        assert_eq!(err, "Line 1: Import cancelled.");
    }

    #[tokio::test]
    async fn phase_one_executes_create_tables_immediately_and_splits_inserts_without_temp_files() {
        use sqlx::Connection;

        let mut conn = Conn::Sqlite(
            sqlx::SqliteConnection::connect("sqlite::memory:")
                .await
                .unwrap(),
        );
        let script = r#"
CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);
INSERT INTO users VALUES (1, 'alice');
CREATE TABLE posts (id INTEGER PRIMARY KEY, user_id INTEGER, title TEXT);
INSERT INTO posts VALUES (10, 1, 'hello');
INSERT INTO users VALUES (2, 'bob');
CREATE INDEX idx_posts_user ON posts (user_id);
"#;
        let cancel = AtomicBool::new(false);
        let track = LoadTrack::planning(1);
        let state = Mutex::new(ProgressState {
            statements: 0,
            bytes: 0,
            total_bytes: 0,
            report: |_progress: LoadProgress| {},
            track: Some(track.clone()),
        });
        let skip = HashSet::new();
        let ctx = RunCtx {
            preamble: &[],
            cancel: &cancel,
            error: &Mutex::new(None),
            progress: &state,
            skip_completed: &skip,
            in_tx: AtomicBool::new(false),
        };
        let mut temps = TempFiles::default();
        let plan = run_phase1(
            &mut conn,
            BufReader::new(Cursor::new(script.as_bytes())),
            Driver::Sqlite,
            1,
            false,
            Vec::new(),
            &ctx,
            &mut temps,
        )
        .await
        .unwrap();

        // Phase 1 already executed both CREATE TABLE statements on the coordinator
        // and kept data chunks in memory without touching disk.
        assert!(temps.paths.is_empty());
        assert_eq!(plan.data_statements, vec![3]);
        assert_eq!(plan.post.len(), 1);
        assert!(plan.post[0].sql.contains("CREATE INDEX"));

        let tables = conn
            .run(
                "SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name",
                10,
                None,
            )
            .await
            .unwrap()
            .text_rows();
        let table_names: Vec<&str> = tables
            .iter()
            .filter_map(|row| row.first().and_then(|cell| cell.as_deref()))
            .collect();
        assert_eq!(table_names, vec!["posts", "users"]);
    }

    #[test]
    fn multithreaded_workers_get_even_insert_counts_without_pre_and_post_inflation() {
        let mut script = String::new();
        // 50 pre-data DDL statements
        for i in 0..50 {
            script.push_str(&format!(
                "DROP TABLE IF EXISTS t{i};\nCREATE TABLE t{i} (id int, body text);\n"
            ));
        }
        // One huge INSERT followed by 15 small INSERTs
        script.push_str(&format!(
            "INSERT INTO t0 VALUES (0, '{}');\n",
            "x".repeat(4000)
        ));
        for i in 1..16 {
            script.push_str(&format!("INSERT INTO t{i} VALUES ({i}, 's');\n"));
        }
        // 20 post-data index statements
        for i in 0..20 {
            script.push_str(&format!("CREATE INDEX idx_t{i} ON t{i} (id);\n"));
        }

        let plan = spool(Driver::Mysql, &script, 4, false);

        // 16 INSERTs across 4 workers -> 4 INSERTs per worker, even though the first INSERT is 4 KB.
        assert_eq!(plan.data_statements, vec![4, 4, 4, 4]);

        let track = LoadTrack::planning(4);
        track.install(&plan.data_statements);
        let work = track.snapshot();
        let worker_totals: Vec<u64> = work.workers.iter().map(|w| w.total_statements).collect();
        let worker_done: Vec<u64> = work.workers.iter().map(|w| w.statements).collect();
        // Thread 1 (worker 0) and overall Phase 2 counts show only the 16 INSERT statements,
        // not the 100 Pre or 20 Post cleanup statements.
        assert_eq!(worker_totals, vec![4, 4, 4, 4]);
        assert_eq!(worker_done, vec![0, 0, 0, 0]);
        assert_eq!(work.statements, 0);
        assert_eq!(work.total_statements, 16);
    }

    #[test]
    fn same_table_inserts_route_to_same_worker_and_idle_workers_take_whole_tables() {
        let script = r#"
INSERT INTO `users` VALUES (1);
INSERT INTO `users` VALUES (2);
INSERT INTO `users` VALUES (3);
INSERT INTO `posts` VALUES (10);
INSERT INTO `posts` VALUES (11);
"#;
        let plan = spool(Driver::Mysql, script, 2, false);

        // All 3 `users` INSERTs stay on worker 0 and both `posts` INSERTs stay on worker 1.
        assert_eq!(plan.data_statements, vec![3, 2]);

        let groups = assign_workers(&plan.data, 2);
        let queues = SharedQueues::new(&groups, false).unwrap();
        let track = LoadTrack::planning(2);
        track.install(&plan.data_statements);

        // Worker 1 runs its own table, then takes `users`, which worker 0 hasn't started.
        let Some(PoppedBatch::Sql(posts)) = queues.pop_batch(1, Some(&track)).unwrap() else {
            panic!("expected the posts batch");
        };
        assert_eq!(posts.len(), 2);
        let Some(PoppedBatch::Sql(users)) = queues.pop_batch(1, Some(&track)).unwrap() else {
            panic!("expected the users batch");
        };
        assert_eq!(users.len(), 3);
        assert!(queues.pop_batch(0, Some(&track)).unwrap().is_none());

        let work = track.snapshot();
        assert_eq!(work.total_statements, 5);
        assert_eq!(work.workers[0].total_statements, 0);
        assert_eq!(work.workers[1].total_statements, 5);
    }

    #[test]
    fn a_started_table_stays_on_its_worker_unless_tables_split() {
        let mut script = String::new();
        for i in 0..(BATCH_MAX_STMTS * 2) {
            script.push_str(&format!("INSERT INTO `users` VALUES ({i});\n"));
        }
        script.push_str("INSERT INTO `posts` VALUES (1);\n");
        for split_tables in [false, true] {
            let plan = spool(Driver::Mysql, &script, 2, split_tables);
            let groups = assign_workers(&plan.data, 2);
            let queues = SharedQueues::new(&groups, split_tables).unwrap();
            let users = groups
                .iter()
                .position(|group| group.iter().any(|chunk| chunk.statements > 1))
                .unwrap();
            let other = 1 - users;
            assert!(queues.pop_batch(users, None).unwrap().is_some());
            assert!(queues.pop_batch(other, None).unwrap().is_some());
            // Worker `other` is out of work, and `users` is partway through its only chunk.
            let stolen = queues.pop_batch(other, None).unwrap();
            assert_eq!(
                stolen.is_some(),
                split_tables,
                "split_tables: {split_tables}"
            );
        }
    }

    #[test]
    fn copy_records_stream_from_the_chunk_and_hand_it_back() {
        let mut bytes = Vec::new();
        write_all(&mut bytes, MAGIC).unwrap();
        write_copy_start(&mut bytes, 0, 1, "COPY t (id) FROM stdin").unwrap();
        write_copy_data(&mut bytes, b"1\n2\n").unwrap();
        write_copy_end(&mut bytes, 26).unwrap();
        write_sql_record(&mut bytes, 1, 5, "INSERT INTO t VALUES (3)").unwrap();
        let chunk = DataChunk {
            table_id: 0,
            source: ChunkSource::Memory(Arc::from(bytes)),
            lines: 5,
            statements: 2,
            bytes: 50,
        };
        let queues = SharedQueues::new(&[vec![chunk]], false).unwrap();
        let Some(PoppedBatch::Copy {
            index,
            sql,
            mut chunk,
            ..
        }) = queues.pop_batch(0, None).unwrap()
        else {
            panic!("expected the COPY");
        };
        assert_eq!((index, sql.as_str()), (0, "COPY t (id) FROM stdin"));
        // No other worker can take the rest of the chunk while its COPY streams.
        assert!(queues.pop_batch(0, None).unwrap().is_none());
        let mut payload = chunk.payload().unwrap();
        let mut rows = Vec::new();
        loop {
            match read_tag(&mut payload).unwrap() {
                Some(TAG_DATA) => rows.extend(read_vec(&mut payload).unwrap()),
                Some(TAG_END) => break,
                other => panic!("unexpected tag {other:?}"),
            }
        }
        assert_eq!(read_u64(&mut payload).unwrap(), 26);
        assert_eq!(rows, b"1\n2\n");
        queues.put_back(0, chunk);
        let Some(PoppedBatch::Sql(rest)) = queues.pop_batch(0, None).unwrap() else {
            panic!("expected the INSERT after the COPY");
        };
        assert_eq!(rest[0].index, 1);
    }

    #[tokio::test]
    async fn sqlite_dump_preserves_autoincrement_sequences_triggers_and_single_worker_progress() {
        use sqlx::Connection;

        let mut conn = Conn::Sqlite(
            sqlx::SqliteConnection::connect("sqlite::memory:")
                .await
                .unwrap(),
        );
        let script = r#"
PRAGMA foreign_keys=OFF;
BEGIN TRANSACTION;
CREATE TABLE [users] (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL);
INSERT OR REPLACE INTO [users] VALUES (1, 'alice');
INSERT INTO "users" VALUES (2, 'bob');
CREATE TABLE audit_log (id INTEGER PRIMARY KEY, msg TEXT);
DELETE FROM sqlite_sequence;
INSERT INTO sqlite_sequence VALUES ('users', 42);
CREATE INDEX idx_users_name ON users (name);
CREATE VIEW user_names AS SELECT name FROM users;
CREATE TRIGGER tr_users_audit AFTER INSERT ON users BEGIN
  INSERT INTO audit_log (msg) VALUES (NEW.name);
END;
COMMIT;
"#;
        let cancel = AtomicBool::new(false);
        let track = LoadTrack::planning(1);
        let mut completed = Vec::new();
        load_sql(
            &mut conn,
            Driver::Sqlite,
            BufReader::new(Cursor::new(script.as_bytes())),
            // Clamped to 1 for SQLite so open_worker is never called
            LoadOptions {
                workers: 4,
                split_tables: false,
            },
            || async { Err::<Conn, String>("SQLite must not open extra workers".into()) },
            &cancel,
            |progress| {
                if let Some(idx) = progress.statement_index {
                    completed.push(idx);
                }
            },
            std::iter::empty::<&str>(),
            &HashSet::new(),
            Some(track.clone()),
        )
        .await
        .unwrap();

        let work = track.snapshot();
        // Phase 2 tracking counts only the 2 data INSERT statements, excluding Phase 1 DDL and Phase 3 cleanup.
        assert_eq!(work.statements, 2);
        assert_eq!(work.total_statements, 2);
        assert_eq!(work.workers.len(), 1);
        assert_eq!(work.workers[0].statements, 2);
        assert_eq!(work.workers[0].total_statements, 2);
        assert_eq!(work.workers[0].state, LoadWorkerState::Done);
        // All 12 statements in the file were still executed and recorded in LoadProgress.
        assert_eq!(completed.len(), 12);

        // Verify trigger did not fire during Phase 2 inserts, but does fire after import
        let audit_before = conn
            .run("SELECT COUNT(*) FROM audit_log", 1, None)
            .await
            .unwrap()
            .text_rows();
        assert_eq!(audit_before[0][0].as_deref(), Some("0"));

        // Verify sqlite_sequence kept the explicit value (42) rather than being wiped by DELETE FROM sqlite_sequence
        conn.execute("INSERT INTO users (name) VALUES ('charlie')")
            .await
            .unwrap();
        let rows = conn
            .run("SELECT id, name FROM users ORDER BY id", 10, None)
            .await
            .unwrap()
            .text_rows();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[2][0].as_deref(), Some("43"));

        let audit_after = conn
            .run("SELECT msg FROM audit_log", 10, None)
            .await
            .unwrap()
            .text_rows();
        assert_eq!(audit_after[0][0].as_deref(), Some("charlie"));

        // Verify table name extraction across Postgres, MySQL, and SQLite forms
        assert_eq!(
            data_target_table(
                r#"INSERT INTO "public"."Users" ("id") OVERRIDING SYSTEM VALUE VALUES (1)"#
            ),
            Some("users".into())
        );
        assert_eq!(
            data_target_table(r#"COPY "app"."audit_events" (id, body) FROM stdin"#),
            Some("audit_events".into())
        );
        assert_eq!(
            data_target_table(r#"COPY public."Users" (id, name) FROM stdin"#),
            Some("users".into())
        );
        assert_eq!(
            data_target_table(r#"INSERT INTO public."Users" VALUES (1)"#),
            Some("users".into())
        );
        assert_eq!(
            data_target_table(r#"INSERT INTO "public".users VALUES (1)"#),
            Some("users".into())
        );
        assert_eq!(
            data_target_table(r#"INSERT INTO "public"."my.table" VALUES (1)"#),
            Some("my.table".into())
        );
        assert_eq!(
            data_target_table("INSERT OR IGNORE INTO [my_table] VALUES (1)"),
            Some("my_table".into())
        );
        assert_eq!(
            decide_phase(
                r#"COMMENT ON COLUMN "active_users"."name" IS 'View column'"#,
                true,
                4
            ),
            Decision::Post
        );

        // Verify GTID SETs, which extra workers don't repeat, and LOCK/UNLOCK TABLES are detected
        assert!(is_mysql_gtid_set(
            "SET @@GLOBAL.GTID_PURGED=/*!80000 '+'*/ 'abc'"
        ));
        assert!(is_mysql_gtid_set("SET @@SESSION.GTID_NEXT= 'AUTOMATIC'"));
        assert!(!is_mysql_gtid_set("SET @@SESSION.SQL_LOG_BIN= 0"));
        assert!(!is_mysql_gtid_set("SET FOREIGN_KEY_CHECKS=0"));
        assert!(replays_on_workers(
            Driver::Mysql,
            "SET @@SESSION.SQL_LOG_BIN= 0"
        ));
        assert!(!replays_on_workers(
            Driver::Mysql,
            "SET @@GLOBAL.GTID_PURGED=/*!80000 '+'*/ 'abc'"
        ));
        assert!(is_lock_or_unlock_tables("LOCK TABLES `users` WRITE"));
        assert!(is_lock_or_unlock_tables("UNLOCK TABLES"));
    }

    #[tokio::test]
    async fn phase_two_progress_tracks_only_inserts_and_excludes_final_cleanup_actions() {
        use sqlx::Connection;

        let mut conn = Conn::Sqlite(
            sqlx::SqliteConnection::connect("sqlite::memory:")
                .await
                .unwrap(),
        );
        let script = r#"
PRAGMA foreign_keys=OFF;
BEGIN TRANSACTION;
CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT);
INSERT INTO items VALUES (1, 'one');
INSERT INTO items VALUES (2, 'two');
INSERT INTO items VALUES (3, 'three');
CREATE INDEX idx_items_label ON items (label);
CREATE VIEW v_items AS SELECT id, label FROM items;
ANALYZE;
COMMIT;
"#;
        let cancel = AtomicBool::new(false);
        let track = LoadTrack::planning(1);
        let report_track = track.clone();
        let mut phase2_snapshots = Vec::new();
        let mut past_phase1 = false;
        load_sql(
            &mut conn,
            Driver::Sqlite,
            BufReader::new(Cursor::new(script.as_bytes())),
            LoadOptions {
                workers: 1,
                split_tables: false,
            },
            || async { Err::<Conn, String>("unused".into()) },
            &cancel,
            |progress| {
                if progress.statement_index.is_none() {
                    past_phase1 = true;
                }
                if past_phase1 {
                    let work = report_track.snapshot();
                    phase2_snapshots.push((
                        progress.statement_index,
                        work.statements,
                        work.total_statements,
                        work.workers[0].statements,
                        work.workers[0].total_statements,
                    ));
                }
            },
            std::iter::empty::<&str>(),
            &HashSet::new(),
            Some(track.clone()),
        )
        .await
        .unwrap();

        // Snapshot 0: finish_phase1 (before any INSERT runs) -> 0 run, 3 total inserts (3 left to run)
        assert_eq!(phase2_snapshots[0], (None, 0, 3, 0, 3));
        // Snapshots 1..=3: each of the 3 INSERT statements -> 1/3, 2/3, 3/3 (100% before cleanup starts)
        assert_eq!(phase2_snapshots[1], (Some(3), 1, 3, 1, 3));
        assert_eq!(phase2_snapshots[2], (Some(4), 2, 3, 2, 3));
        assert_eq!(phase2_snapshots[3], (Some(5), 3, 3, 3, 3));
        // Snapshots 4..=7: final cleanup (CREATE INDEX, CREATE VIEW, ANALYZE, COMMIT) do not inflate total or done counts
        for &(stmt_idx, done, total, worker_done, worker_total) in &phase2_snapshots[4..] {
            assert!(stmt_idx.unwrap() >= 6);
            assert_eq!((done, total, worker_done, worker_total), (3, 3, 3, 3));
        }
    }

    #[test]
    fn tables_are_weighted_by_lines_to_import_before_thread_assignment() {
        // `tiny` appears first with 2 single-line INSERTs (2 lines total), followed by
        // `orders` (1 multi-line INSERT spanning 101 lines) and `order_items` (1 multi-line
        // INSERT spanning 91 lines). Online assignment by statement count would put both
        // 101-line and 91-line tables onto worker 1 because worker 0 had 2 statements.
        // Weighting tables by lines to import before thread assignment places `orders` (101 lines)
        // on worker 0 and `order_items` + `tiny` (91 + 2 = 93 lines) on worker 1.
        let mut script = String::from(
            "INSERT INTO `tiny` VALUES (1);\n\
             INSERT INTO `tiny` VALUES (2);\n",
        );
        script.push_str("INSERT INTO `orders` VALUES\n");
        for i in 0..100 {
            let term = if i == 99 { ";\n" } else { ",\n" };
            script.push_str(&format!("  ({i}){term}"));
        }
        script.push_str("INSERT INTO `order_items` VALUES\n");
        for i in 0..90 {
            let term = if i == 89 { ";\n" } else { ",\n" };
            script.push_str(&format!("  ({i}){term}"));
        }

        let plan = spool(Driver::Mysql, &script, 2, false);

        // Each table gets its own chunk in Phase 1 with exact line-range weights.
        assert_eq!(plan.data.len(), 3);
        let chunk_lines: Vec<u64> = plan.data.iter().map(|c| c.lines).collect();
        assert_eq!(chunk_lines, vec![2, 101, 91]);

        let groups = assign_workers(&plan.data, 2);
        assert_eq!(groups.len(), 2);
        let worker_lines: Vec<u64> = groups
            .iter()
            .map(|group| group.iter().map(|c| c.lines).sum())
            .collect();
        // Worker 0 gets `orders` (101 lines); Worker 1 gets `order_items` + `tiny` (91 + 2 = 93 lines).
        assert_eq!(worker_lines, vec![101, 93]);
        // Worker 0 has 1 statement (`orders`); Worker 1 has 3 statements (`order_items` + 2 `tiny`).
        assert_eq!(plan.data_statements, vec![1, 3]);

        // Also verify 4 tables with (10, 20, 100, 90) lines balance evenly (110 vs 110 lines) across 2 threads.
        let mut even_script = String::new();
        for (table, count) in [("t_a", 10), ("t_b", 20), ("t_c", 100), ("t_d", 90)] {
            for i in 0..count {
                even_script.push_str(&format!("INSERT INTO {table} VALUES ({i});\n"));
            }
        }
        let plan_even = spool(Driver::Mysql, &even_script, 2, false);
        let even_groups = assign_workers(&plan_even.data, 2);
        let even_worker_lines: Vec<u64> = even_groups
            .iter()
            .map(|group| group.iter().map(|c| c.lines).sum())
            .collect();
        assert_eq!(even_worker_lines, vec![110, 110]);
        assert_eq!(plan_even.data_statements, vec![110, 110]);

        // And verify Postgres COPY ... FROM stdin blocks weight tables by COPY line span.
        let mut pg_script = String::from(
            "COPY public.roles (id) FROM stdin;\n1\n2\n\\.\n\
             COPY public.users (id) FROM stdin;\n",
        );
        for i in 0..50 {
            pg_script.push_str(&format!("{i}\n"));
        }
        pg_script.push_str("\\.\nCOPY public.events (id) FROM stdin;\n");
        for i in 0..45 {
            pg_script.push_str(&format!("{i}\n"));
        }
        pg_script.push_str("\\.\n");

        let pg_plan = spool(Driver::Postgres, &pg_script, 2, false);
        let pg_groups = assign_workers(&pg_plan.data, 2);
        let pg_worker_lines: Vec<u64> = pg_groups
            .iter()
            .map(|group| group.iter().map(|c| c.lines).sum())
            .collect();
        // `users` (1 COPY line + 50 data lines = 51) goes to worker 0;
        // `events` (1 + 45 = 46) and `roles` (1 + 2 = 3) go to worker 1 (49 lines).
        assert_eq!(pg_worker_lines, vec![51, 49]);
        assert_eq!(pg_plan.data_statements, vec![1, 2]);
    }

    #[tokio::test]
    async fn batch_execution_falls_back_one_by_one_and_reports_exact_failing_line() {
        use sqlx::Connection;

        let mut conn = Conn::Sqlite(
            sqlx::SqliteConnection::connect("sqlite::memory:")
                .await
                .unwrap(),
        );
        // 4 INSERTs in Phase 2 where the 3rd INSERT violates UNIQUE constraint on line 5.
        let script = "\
CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT);\n\
INSERT INTO items VALUES (1, 'one');\n\
INSERT INTO items VALUES (2, 'two');\n\
INSERT INTO items VALUES (1, 'dup');\n\
INSERT INTO items VALUES (3, 'three');\n\
";
        let cancel = AtomicBool::new(false);
        let track = LoadTrack::planning(1);
        let mut completed = Vec::new();
        let err = load_sql(
            &mut conn,
            Driver::Sqlite,
            BufReader::new(Cursor::new(script.as_bytes())),
            LoadOptions {
                workers: 1,
                split_tables: false,
            },
            || async { Err::<Conn, String>("unused".into()) },
            &cancel,
            |progress| {
                if let Some(idx) = progress.statement_index {
                    completed.push(idx);
                }
            },
            std::iter::empty::<&str>(),
            &HashSet::new(),
            Some(track),
        )
        .await
        .unwrap_err();

        match err {
            LoadSqlError::Failed(msg) => {
                assert!(
                    msg.starts_with("Line 4:"),
                    "expected error on Line 4, got: {msg}"
                );
            }
            other => panic!("expected LoadSqlError::Failed, got {other:?}"),
        }
        // Statements 0 (CREATE), 1 (INSERT 1), and 2 (INSERT 2) succeeded before statement 3 failed.
        assert_eq!(completed, vec![0, 1, 2]);

        let rows = conn
            .run("SELECT id, label FROM items ORDER BY id", 10, None)
            .await
            .unwrap()
            .text_rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0].as_deref(), Some("1"));
        assert_eq!(rows[1][0].as_deref(), Some("2"));
    }

    #[test]
    fn single_table_large_dump_splits_across_multiple_workers() {
        let total = (TABLE_CHUNK_STMTS as usize) * 2 + 100;
        let mut script = String::with_capacity(total * 36);
        for i in 0..total {
            script.push_str(&format!("INSERT INTO `events` VALUES ({i});\n"));
        }

        let plan = spool(Driver::Mysql, &script, 4, true);
        let whole = spool(Driver::Mysql, &script, 4, false);
        assert_eq!(whole.data.len(), 1);

        // Single table exceeding TABLE_CHUNK_STMTS splits into 3 chunks with the same table_id.
        assert_eq!(plan.data.len(), 3);
        assert!(plan.data.iter().all(|c| c.table_id == 0));
        let chunk_stmts: Vec<u64> = plan.data.iter().map(|c| c.statements).collect();
        assert_eq!(chunk_stmts, vec![TABLE_CHUNK_STMTS, TABLE_CHUNK_STMTS, 100]);

        // Since unique_tables (1) < workers (4), chunks are distributed across 3 workers.
        let groups = assign_workers(&plan.data, 4);
        assert_eq!(groups.len(), 3);
        assert_eq!(
            plan.data_statements,
            vec![TABLE_CHUNK_STMTS, TABLE_CHUNK_STMTS, 100]
        );

        // Also verify byte-threshold splitting for a single table.
        let mut split = Spool::new(4, true);
        let b0 = split.bin_for(Some("events".into()));
        assert_eq!(b0, 0);
        split.bins[b0].bytes = TABLE_CHUNK_BYTES;
        split.bins[b0].statements = 1;
        let b1 = split.bin_for(Some("events".into()));
        assert_eq!(b1, 1);
        assert_eq!(split.bins[b0].table_id, split.bins[b1].table_id);
        let mut whole = Spool::new(4, false);
        let b0 = whole.bin_for(Some("events".into()));
        whole.bins[b0].bytes = TABLE_CHUNK_BYTES;
        assert_eq!(whole.bin_for(Some("events".into())), b0);
    }

    #[tokio::test]
    async fn phase_one_parallelizes_table_ddl_across_multiple_workers_and_reuses_connections() {
        use sqlx::sqlite::SqliteConnectOptions;
        use sqlx::Connection;
        use std::str::FromStr;

        // Verify `pre_target_table` extracts independent table names and treats shared/dependent DDL as barriers.
        assert_eq!(
            pre_target_table(Driver::Mysql, "DROP TABLE IF EXISTS `Users`"),
            Some("users".into())
        );
        assert_eq!(
            pre_target_table(Driver::Mysql, "CREATE TABLE `Users` (`id` int PRIMARY KEY)"),
            Some("users".into())
        );
        assert_eq!(
            pre_target_table(
                Driver::Mysql,
                "/*!40000 ALTER TABLE `Users` DISABLE KEYS */"
            ),
            Some("users".into())
        );
        assert_eq!(
            pre_target_table(
                Driver::Postgres,
                r#"CREATE TABLE public."AuditEvents" (id integer NOT NULL)"#
            ),
            Some("auditevents".into())
        );
        assert_eq!(
            pre_target_table(
                Driver::Postgres,
                r#"ALTER TABLE ONLY public."AuditEvents" ALTER COLUMN id SET DEFAULT 1"#
            ),
            Some("auditevents".into())
        );
        assert_eq!(
            pre_target_table(
                Driver::Postgres,
                r#"COMMENT ON TABLE public."AuditEvents" IS 'events'"#
            ),
            Some("auditevents".into())
        );
        // Barriers: multi-table DROP, TEMPORARY tables, Postgres inline REFERENCES, CREATE SEQUENCE, SET
        assert_eq!(pre_target_table(Driver::Mysql, "DROP TABLE a, b"), None);
        assert_eq!(
            pre_target_table(Driver::Mysql, "CREATE TEMPORARY TABLE tmp (id int)"),
            None
        );
        assert_eq!(
            pre_target_table(
                Driver::Postgres,
                "CREATE TABLE child (id int, parent_id int REFERENCES parent(id))"
            ),
            None
        );
        assert_eq!(
            pre_target_table(
                Driver::Mysql,
                "CREATE TABLE `cost_data_items` (`id` bigint unsigned NOT NULL, `cost_data_group_id` int unsigned NOT NULL, `cost_data_task_id` int unsigned NOT NULL, CONSTRAINT `fk_group` FOREIGN KEY (`cost_data_group_id`) REFERENCES `cost_data_groups` (`id`), CONSTRAINT `fk_task` FOREIGN KEY (`cost_data_task_id`) REFERENCES `cost_data_tasks` (`id`))"
            ),
            None
        );
        let mut fk_tables = HashSet::new();
        collect_fk_tables(
            "CREATE TABLE `cost_data_items` (`id` bigint, CONSTRAINT `fk_group` FOREIGN KEY (`cost_data_group_id`) REFERENCES `cost_data_groups` (`id`), CONSTRAINT `fk_task` FOREIGN KEY (`cost_data_task_id`) REFERENCES `cost_data_tasks` (`id`))",
            &mut fk_tables,
        );
        assert!(fk_tables.contains("cost_data_items"));
        assert!(fk_tables.contains("cost_data_groups"));
        assert!(fk_tables.contains("cost_data_tasks"));
        assert!(is_retryable_deadlock(
            "Deadlock found when trying to get lock; try restarting transaction"
        ));
        assert!(is_retryable_deadlock(
            "Lock wait timeout exceeded; try restarting transaction"
        ));
        assert!(is_retryable_deadlock("ERROR: deadlock detected"));
        assert!(!is_retryable_deadlock(
            "Duplicate entry '1' for key 'PRIMARY'"
        ));
        assert_eq!(
            pre_target_table(Driver::Postgres, "CREATE SEQUENCE users_id_seq"),
            None
        );
        assert_eq!(pre_target_table(Driver::Mysql, "SET NAMES utf8mb4"), None);

        // Verify `run_phase1_parallel` distributes table DDL across multiple worker connections,
        // keeps same-table DROP + CREATE on the same worker, updates LoadTrack per-worker Phase 1
        // counts, and returns the opened extra connections for reuse in Phase 2.
        let dir = std::env::temp_dir().join(format!(
            "recon-phase1-parallel-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("test.sqlite");
        let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", db_path.display()))
            .unwrap()
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .busy_timeout(std::time::Duration::from_secs(5));

        let mut coord = Conn::Sqlite(sqlx::SqliteConnection::connect_with(&opts).await.unwrap());
        let script = r#"
DROP TABLE IF EXISTS alpha;
CREATE TABLE alpha (id INTEGER PRIMARY KEY, v TEXT);
DROP TABLE IF EXISTS beta;
CREATE TABLE beta (id INTEGER PRIMARY KEY, v TEXT);
DROP TABLE IF EXISTS gamma;
CREATE TABLE gamma (id INTEGER PRIMARY KEY, v TEXT);
DROP TABLE IF EXISTS delta;
CREATE TABLE delta (id INTEGER PRIMARY KEY, v TEXT);
INSERT INTO alpha VALUES (1, 'a');
INSERT INTO beta VALUES (2, 'b');
INSERT INTO gamma VALUES (3, 'c');
INSERT INTO delta VALUES (4, 'd');
"#;
        let mut extras = Vec::new();
        for _ in 0..2 {
            extras.push(Conn::Sqlite(
                sqlx::SqliteConnection::connect_with(&opts).await.unwrap(),
            ));
        }

        let cancel = AtomicBool::new(false);
        let track = LoadTrack::planning(3);
        let state = Mutex::new(ProgressState {
            statements: 0,
            bytes: 0,
            total_bytes: 0,
            report: |_progress: LoadProgress| {},
            track: Some(track.clone()),
        });
        let skip = HashSet::new();
        let ctx = RunCtx {
            preamble: &[],
            cancel: &cancel,
            error: &Mutex::new(None),
            progress: &state,
            skip_completed: &skip,
            in_tx: AtomicBool::new(false),
        };
        let mut temps = TempFiles::default();
        let mut plan = run_phase1_parallel(
            &mut coord,
            BufReader::new(Cursor::new(script.as_bytes())),
            Driver::Mysql,
            3,
            false,
            extras,
            &ctx,
            &mut temps,
        )
        .await
        .unwrap();

        // 4 tables (8 DDL statements) across 3 workers, and both extra connections come back for Phase 2
        assert_eq!(plan.extras.len(), 2);

        // All 3 worker threads participated in Phase 1 and recorded their completed DDL statements
        let phase1_work = track.snapshot();
        let worker_ddl_done: u64 = phase1_work.workers.iter().map(|w| w.statements).sum();
        let worker_ddl_total: u64 = phase1_work.workers.iter().map(|w| w.total_statements).sum();
        assert_eq!(worker_ddl_done, 8);
        assert_eq!(worker_ddl_total, 8);
        assert!(phase1_work.workers.iter().all(|w| w.statements >= 2));

        // Phase 2 reuses the 2 extra connections
        track.install(&plan.data_statements);
        let pre_extras = std::mem::take(&mut plan.extras);
        run_phase2(&mut coord, &plan.data, pre_extras, &[], false, &ctx)
            .await
            .unwrap();

        let rows = coord
            .run(
                "SELECT (SELECT v FROM alpha) || (SELECT v FROM beta) || (SELECT v FROM gamma) || (SELECT v FROM delta)",
                1,
                None,
            )
            .await
            .unwrap()
            .text_rows();
        assert_eq!(rows[0][0].as_deref(), Some("abcd"));

        coord.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn work_stealing_steals_untouched_table_chunk_from_back_before_active_front_table() {
        let script = "\
INSERT INTO `table_a` VALUES (1);\n\
INSERT INTO `table_a` VALUES (2);\n\
INSERT INTO `table_a` VALUES (3);\n\
INSERT INTO `table_b` VALUES (10);\n\
INSERT INTO `table_b` VALUES (20);\n\
INSERT INTO `table_c` VALUES (100);\n\
";
        let plan = spool(Driver::Mysql, script, 2, false);

        let groups = assign_workers(&plan.data, 2);
        // Worker 0 has 1 table (`table_a`, 3 stmts); Worker 1 has 2 tables (`table_b` at front, `table_c` at back).
        assert_eq!(groups[0].len(), 1);
        assert_eq!(groups[1].len(), 2);

        let queues = SharedQueues::new(&groups, false).unwrap();
        let track = LoadTrack::planning(2);
        track.install(&plan.data_statements);

        // Worker 0 finishes its own `table_a` batch.
        let PoppedBatch::Sql(w0_first) = queues.pop_batch(0, Some(&track)).unwrap().unwrap() else {
            panic!("expected SQL batch");
        };
        assert_eq!(w0_first.len(), 3);
        assert!(w0_first[0].sql.contains("table_a"));

        // Worker 0 is now idle while Worker 1 still has `table_b` (front) and `table_c` (back).
        // Worker 0 steals `table_c` from the back of Worker 1's queue instead of colliding on `table_b`.
        let PoppedBatch::Sql(w0_stolen) = queues.pop_batch(0, Some(&track)).unwrap().unwrap()
        else {
            panic!("expected stolen SQL batch");
        };
        assert_eq!(w0_stolen.len(), 1);
        assert!(w0_stolen[0].sql.contains("table_c"));

        // Worker 1 still gets its own front `table_b` batch undisturbed.
        let PoppedBatch::Sql(w1_own) = queues.pop_batch(1, Some(&track)).unwrap().unwrap() else {
            panic!("expected Worker 1 front SQL batch");
        };
        assert_eq!(w1_own.len(), 2);
        assert!(w1_own[0].sql.contains("table_b"));
    }

    #[test]
    fn only_known_dump_headers_load_in_parallel() {
        for head in [
            "-- MySQL dump 10.13  Distrib 8.0.36",
            "-- MariaDB dump 10.19",
            "--\n-- PostgreSQL database dump\n--",
            "-- Recon SQL export",
            "-- Recon backup v1",
            "\u{feff}-- MySQL dump 10.13",
        ] {
            let file = format!("{head}\nCREATE TABLE a (id int);\nINSERT INTO a VALUES (1);\n");
            let mut reader = Cursor::new(file.as_bytes());
            assert!(starts_like_dump(&mut reader).unwrap(), "{head}");
            assert_eq!(reader.position(), 0, "the head is left for the load");
        }
        for file in [
            "CREATE TABLE a (id int);\nINSERT INTO a VALUES (1);\n",
            "-- my notes\nCREATE TABLE a (id int);\n",
            "",
        ] {
            assert!(
                !starts_like_dump(&mut Cursor::new(file.as_bytes())).unwrap(),
                "{file}"
            );
        }
    }

    #[test]
    fn drop_create_and_alter_follow_what_they_change() {
        assert_eq!(
            decide_phase("DROP TABLE IF EXISTS `a`", true, 2),
            Decision::Pre
        );
        assert_eq!(decide_phase("TRUNCATE `a`", true, 2), Decision::Pre);
        for sql in [
            "DROP VIEW IF EXISTS v",
            "DROP MATERIALIZED VIEW m",
            "DROP TRIGGER t",
            "DROP INDEX i",
            "DROP FUNCTION f",
            "DROP PROCEDURE p",
            "CREATE FUNCTION f() RETURNS int RETURN 1",
            "CREATE PROCEDURE p() SELECT 1",
        ] {
            assert_eq!(decide_phase(sql, false, 2), Decision::Pre, "{sql}");
            assert_eq!(decide_phase(sql, true, 2), Decision::Post, "{sql}");
        }
        assert_eq!(
            decide_phase("ALTER VIEW v AS SELECT 1", false, 2),
            Decision::Post
        );
        assert_eq!(
            decide_phase("ALTER MATERIALIZED VIEW m OWNER TO app", false, 2),
            Decision::Post
        );
    }

    fn stmts(sqls: &[&str]) -> Vec<SqlStmt> {
        sqls.iter()
            .enumerate()
            .map(|(index, sql)| SqlStmt {
                index,
                line: index + 1,
                sql: sql.to_string(),
            })
            .collect()
    }

    async fn count(conn: &mut Conn, table: &str) -> String {
        conn.run(&format!("SELECT COUNT(*) FROM {table}"), 1, None)
            .await
            .unwrap()
            .text_rows()[0][0]
            .clone()
            .unwrap()
    }

    #[tokio::test]
    async fn a_batch_that_fails_as_a_whole_still_loads_one_statement_at_a_time() {
        use sqlx::Connection;

        let mut conn = Conn::Sqlite(
            sqlx::SqliteConnection::connect("sqlite::memory:")
                .await
                .unwrap(),
        );
        conn.execute("CREATE TABLE t (id INTEGER PRIMARY KEY)")
            .await
            .unwrap();
        // The wrapping BEGIN fails inside an open transaction, but each INSERT is fine on its own.
        conn.execute("BEGIN").await.unwrap();
        let batch = stmts(&["INSERT INTO t VALUES (1)", "INSERT INTO t VALUES (2)"]);
        let mut done = Vec::new();
        run_batch(
            &mut conn,
            &batch,
            true,
            &mut String::new(),
            &AtomicBool::new(false),
            |ran| done.extend(ran.iter().map(|stmt| stmt.index)),
        )
        .await
        .unwrap();
        assert_eq!(done, vec![0, 1]);
        assert_eq!(count(&mut conn, "t").await, "2");

        // A statement that fails on its own is the one reported, after the ones before it ran.
        let batch = stmts(&[
            "INSERT INTO t VALUES (3)",
            "INSERT INTO t VALUES (1)",
            "INSERT INTO t VALUES (4)",
        ]);
        let mut done = Vec::new();
        let err = run_batch(
            &mut conn,
            &batch,
            true,
            &mut String::new(),
            &AtomicBool::new(false),
            |ran| done.extend(ran.iter().map(|stmt| stmt.index)),
        )
        .await
        .unwrap_err();
        assert_eq!(done, vec![0]);
        assert!(err.to_string().contains("Line 2"), "{err}");
    }

    #[tokio::test]
    async fn an_unwrapped_batch_leaves_the_files_transaction_open() {
        use sqlx::Connection;

        let mut conn = Conn::Sqlite(
            sqlx::SqliteConnection::connect("sqlite::memory:")
                .await
                .unwrap(),
        );
        conn.execute("CREATE TABLE t (id INTEGER PRIMARY KEY)")
            .await
            .unwrap();
        conn.execute("BEGIN").await.unwrap();
        conn.execute("INSERT INTO t VALUES (1)").await.unwrap();
        let batch = stmts(&["INSERT INTO t VALUES (2)", "INSERT INTO t VALUES (3)"]);
        run_batch(
            &mut conn,
            &batch,
            false,
            &mut String::new(),
            &AtomicBool::new(false),
            |_| {},
        )
        .await
        .unwrap();
        conn.execute("ROLLBACK").await.unwrap();
        assert_eq!(count(&mut conn, "t").await, "0");
    }
}
