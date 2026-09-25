use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use uuid::Uuid;

pub const QUERY_LOG_EVENT: &str = "query-log";
pub const QUERY_LOG_CLEARED_EVENT: &str = "query-log-cleared";

const MAX_ENTRIES: usize = 4000;
const MAX_SQL_BYTES: usize = 12_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QueryOrigin {
    Editor,
    Browse,
    Schema,
    Edit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryLogEntry {
    pub id: String,
    pub at: u64,
    pub connection: String,
    pub driver: String,
    pub database: String,
    pub sql: String,
    pub origin: QueryOrigin,
    pub success: bool,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u64>,
    #[serde(default)]
    pub error: String,
}

pub struct QueryRecord<'a> {
    pub connection: &'a str,
    pub driver: &'a str,
    pub database: &'a str,
    pub sql: &'a str,
    pub origin: QueryOrigin,
    pub duration: Duration,
    pub outcome: Result<Option<u64>, &'a str>,
}

struct Logger {
    path: PathBuf,
    entries: Vec<QueryLogEntry>,
    app: Option<AppHandle>,
    paused: bool,
}

static LOGGER: Mutex<Option<Logger>> = Mutex::new(None);

pub fn init(path: PathBuf, app: Option<AppHandle>) {
    let entries = load_entries(&path);
    if let Ok(mut slot) = LOGGER.lock() {
        *slot = Some(Logger {
            path,
            entries,
            app,
            paused: false,
        });
    }
}

pub fn list() -> Vec<QueryLogEntry> {
    LOGGER
        .lock()
        .ok()
        .and_then(|slot| slot.as_ref().map(|logger| logger.entries.clone()))
        .unwrap_or_default()
}

pub fn paused() -> bool {
    LOGGER
        .lock()
        .ok()
        .and_then(|slot| slot.as_ref().map(|logger| logger.paused))
        .unwrap_or(false)
}

pub fn set_paused(paused: bool) -> Result<(), String> {
    let mut slot = LOGGER
        .lock()
        .map_err(|_| "Could not lock the query history.".to_string())?;
    if let Some(logger) = slot.as_mut() {
        logger.paused = paused;
    }
    Ok(())
}

pub fn clear() -> Result<(), String> {
    let mut slot = LOGGER
        .lock()
        .map_err(|_| "Could not lock the query history.".to_string())?;
    let Some(logger) = slot.as_mut() else {
        return Ok(());
    };
    logger.entries.clear();
    fs::write(&logger.path, "").map_err(|err| format!("Could not clear query history: {err}"))?;
    if let Some(app) = &logger.app {
        let _ = app.emit(QUERY_LOG_CLEARED_EVENT, ());
    }
    Ok(())
}

pub fn record(record: QueryRecord<'_>) {
    let Ok(mut slot) = LOGGER.lock() else {
        return;
    };
    let Some(logger) = slot.as_mut() else {
        return;
    };
    if logger.paused {
        return;
    }
    let (success, rows, error) = match record.outcome {
        Ok(rows) => (true, rows, String::new()),
        Err(message) => (false, None, message.to_string()),
    };
    let entry = QueryLogEntry {
        id: Uuid::new_v4().to_string(),
        at: now_ms(),
        connection: record.connection.to_string(),
        driver: record.driver.to_string(),
        database: record.database.to_string(),
        sql: truncate(record.sql),
        origin: record.origin,
        success,
        duration_ms: u64::try_from(record.duration.as_millis()).unwrap_or(u64::MAX),
        rows,
        error,
    };
    logger.entries.push(entry.clone());
    if logger.entries.len() > MAX_ENTRIES {
        let drop_count = logger.entries.len() - MAX_ENTRIES;
        logger.entries.drain(0..drop_count);
        let _ = rewrite(&logger.path, &logger.entries);
    } else {
        let _ = append_line(&logger.path, &entry);
    }
    if let Some(app) = &logger.app {
        let _ = app.emit(QUERY_LOG_EVENT, &entry);
    }
}

fn truncate(text: &str) -> String {
    let text = text.trim();
    if text.len() <= MAX_SQL_BYTES {
        return text.to_string();
    }
    let mut end = MAX_SQL_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n… truncated {} bytes",
        &text[..end],
        text.len().saturating_sub(end)
    )
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn load_entries(path: &Path) -> Vec<QueryLogEntry> {
    let Ok(file) = fs::File::open(path) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else {
            continue;
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<QueryLogEntry>(line) {
            entries.push(entry);
        }
    }
    if entries.len() > MAX_ENTRIES {
        let drop_count = entries.len() - MAX_ENTRIES;
        entries.drain(0..drop_count);
    }
    entries
}

fn append_line(path: &Path, entry: &QueryLogEntry) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|err| err.to_string())?;
    let raw = serde_json::to_string(entry).map_err(|err| err.to_string())?;
    writeln!(file, "{raw}").map_err(|err| err.to_string())
}

fn rewrite(path: &Path, entries: &[QueryLogEntry]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let mut raw = String::new();
    for entry in entries {
        raw.push_str(&serde_json::to_string(entry).map_err(|err| err.to_string())?);
        raw.push('\n');
    }
    fs::write(path, raw).map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn temp_log() -> PathBuf {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "recon-history-{}-{}",
            std::process::id(),
            n
        ));
        let _ = fs::create_dir_all(&dir);
        dir.join("query-history.jsonl")
    }

    fn sample<'a>(connection: &'a str, sql: &'a str, outcome: Result<Option<u64>, &'a str>) -> QueryRecord<'a> {
        QueryRecord {
            connection,
            driver: "PostgreSQL",
            database: "public",
            sql,
            origin: QueryOrigin::Editor,
            duration: Duration::from_millis(12),
            outcome,
        }
    }

    #[test]
    fn persists_pauses_and_clears_query_history() {
        let path = temp_log();
        init(path.clone(), None);
        set_paused(false).unwrap();
        record(sample("history-a", "SELECT 1", Ok(Some(1))));
        record(sample("history-a", "SELEC 1", Err("syntax error")));

        let mine: Vec<_> = list()
            .into_iter()
            .filter(|entry| entry.connection == "history-a")
            .collect();
        assert!(mine.iter().any(|entry| entry.sql == "SELECT 1" && entry.rows == Some(1)));
        assert!(mine.iter().any(|entry| !entry.success && entry.error == "syntax error"));

        init(path, None);
        assert!(list().iter().any(|entry| entry.connection == "history-a"));

        set_paused(true).unwrap();
        record(sample("history-paused", "SELECT 2", Ok(None)));
        assert!(!list().iter().any(|entry| entry.connection == "history-paused"));
        set_paused(false).unwrap();

        clear().unwrap();
        assert!(!list().iter().any(|entry| entry.connection == "history-a"));
    }
}
