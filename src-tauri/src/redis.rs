//! Redis connections: a key browser, value editors, and a command console.
//!
//! Redis has no tables or SQL, so it keeps its own sessions here instead of
//! going through the SQL `Session`, `Pool`, and `Dialect` in `db`. It shares
//! the connection settings, passwords, TLS material, and SSH tunnels.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use redis::aio::{ConnectionLike, MultiplexedConnection};
use redis::{
    AsyncConnectionConfig, Client, ClientTlsConfig, Cmd, ConnectionAddr, FromRedisValue,
    IntoConnectionInfo, Pipeline, RedisConnectionInfo, RedisError, TlsCertificates, Value,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};
use tokio::sync::Mutex;

use crate::commands::AppState;
use crate::db::ssh::{self, Tunnel};
use crate::db::tls::{self, TlsMaterial};
use crate::db_commands::{explain, open_tunnel, password_for};
use crate::models::{ConnectionEntry, Driver};
use crate::prompts::Prompter;
use crate::query_log::{self, QueryOrigin, QueryRecord};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
/// Blocking console commands such as `BLPOP key 0` give up after this long.
const CONSOLE_TIMEOUT: Duration = Duration::from_secs(60);
/// How much of a string value is loaded for viewing.
const STRING_PREVIEW: usize = 1 << 20;
const PAGE_MAX: u32 = 1_000;
const SCAN_COUNT_MAX: u32 = 10_000;
/// How long one key-list page keeps scanning for matches before returning what it found.
const SCAN_BUDGET: Duration = Duration::from_secs(2);
const DEFAULT_DATABASES: u32 = 16;
/// Console commands that would take over the connection or leave Recon's view.
const REFUSED: &[(&str, &str)] = &[
    (
        "MONITOR",
        "MONITOR streams forever, so the console can't run it.",
    ),
    ("SUBSCRIBE", "Pub/Sub isn't available in the console yet."),
    ("PSUBSCRIBE", "Pub/Sub isn't available in the console yet."),
    ("SSUBSCRIBE", "Pub/Sub isn't available in the console yet."),
    ("SYNC", "Replication commands can't run in the console."),
    ("PSYNC", "Replication commands can't run in the console."),
    ("SELECT", "Switch databases with the database menu instead."),
    ("QUIT", "Close the tab to disconnect."),
    (
        "RESET",
        "RESET would sign the console out. Close the tab to disconnect.",
    ),
];

#[derive(Default)]
pub struct RedisStore(Mutex<HashMap<String, Arc<RedisSession>>>);

impl RedisStore {
    async fn get(&self, id: &str) -> Result<Arc<RedisSession>, String> {
        self.0
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| "This connection is closed.".to_string())
    }
}

pub struct RedisSession {
    entry: ConnectionEntry,
    /// Where to connect: the server, or the local end of the SSH tunnel.
    target: ConnectionEntry,
    password: Option<String>,
    tls: TlsMaterial,
    tunnel: Option<Arc<Tunnel>>,
    live: Mutex<Live>,
}

struct Live {
    db: u32,
    conn: MultiplexedConnection,
    /// Its own connection, so a blocking command can't hold up the key browser.
    console: Option<MultiplexedConnection>,
}

fn message(err: RedisError) -> String {
    if err.is_timeout() {
        return "Redis didn't answer in time.".into();
    }
    err.to_string()
}

fn dropped(err: &RedisError) -> bool {
    err.is_connection_dropped() || err.is_io_error() || err.is_unrecoverable_error()
}

fn client_for(
    target: &ConnectionEntry,
    password: Option<&str>,
    material: &TlsMaterial,
    db: u32,
) -> Result<Client, String> {
    let host = target.host.clone();
    let port = target.port;
    let secure = target.requires_tls();
    let addr = if secure {
        ConnectionAddr::TcpTls {
            host,
            port,
            insecure: !target.verifies_certificate(),
            tls_params: None,
        }
    } else {
        ConnectionAddr::Tcp(host, port)
    };
    let mut settings = RedisConnectionInfo::default().set_db(i64::from(db));
    if !target.user.is_empty() {
        settings = settings.set_username(&target.user);
    }
    if let Some(password) = password.filter(|value| !value.is_empty()) {
        settings = settings.set_password(password);
    }
    let info = addr
        .into_connection_info()
        .map_err(message)?
        .set_redis_settings(settings);
    let client_tls = match (&material.cert, &material.key) {
        (Some(cert), Some(key)) => Some(ClientTlsConfig {
            client_cert: cert.clone(),
            client_key: key.clone(),
        }),
        _ => None,
    };
    if secure && (client_tls.is_some() || material.ca.is_some()) {
        let certificates = TlsCertificates {
            client_tls,
            root_cert: material.ca.clone(),
        };
        Client::build_with_tls(info, certificates).map_err(message)
    } else {
        Client::open(info).map_err(message)
    }
}

async fn open_connection(
    client: &Client,
    response_timeout: Duration,
) -> Result<MultiplexedConnection, String> {
    let config = AsyncConnectionConfig::new()
        .set_connection_timeout(Some(CONNECT_TIMEOUT))
        .set_response_timeout(Some(response_timeout));
    client
        .get_multiplexed_async_connection_with_config(&config)
        .await
        .map_err(message)
}

fn parse_info(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.trim().split_once(':'))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

async fn server_version(conn: &mut MultiplexedConnection) -> Result<String, String> {
    let info: String = redis::cmd("INFO")
        .arg("server")
        .query_async(conn)
        .await
        .map_err(message)?;
    let info = parse_info(&info);
    let version = info.get("redis_version").cloned().unwrap_or_default();
    let mode = info.get("redis_mode").map(String::as_str).unwrap_or("");
    Ok(match mode {
        "" | "standalone" => version,
        mode => format!("{version} ({mode})"),
    })
}

/// Called by `test_connection` once the password, TLS, and tunnel are ready.
pub async fn test(
    target: &ConnectionEntry,
    password: Option<&str>,
    material: &TlsMaterial,
) -> Result<String, String> {
    let db = target.database.parse().unwrap_or(0);
    let client = client_for(target, password, material, db)?;
    let mut conn = open_connection(&client, RESPONSE_TIMEOUT).await?;
    let version = server_version(&mut conn).await?;
    Ok(format!("Connected to Redis {version}"))
}

impl RedisSession {
    async fn connect(&self, db: u32, timeout: Duration) -> Result<MultiplexedConnection, String> {
        let client = client_for(&self.target, self.password.as_deref(), &self.tls, db)?;
        open_connection(&client, timeout)
            .await
            .map_err(|err| explain(self.tunnel.as_deref(), err))
    }

    async fn conn(&self) -> (MultiplexedConnection, u32) {
        let live = self.live.lock().await;
        (live.conn.clone(), live.db)
    }

    async fn reconnect(&self, db: u32) -> Result<MultiplexedConnection, String> {
        let conn = self.connect(db, RESPONSE_TIMEOUT).await?;
        let mut live = self.live.lock().await;
        if live.db == db {
            live.conn = conn.clone();
        }
        Ok(conn)
    }

    /// Runs a read, reconnecting once if the connection dropped.
    async fn read<T: FromRedisValue>(&self, cmd: &Cmd) -> Result<T, String> {
        let (mut conn, db) = self.conn().await;
        match cmd.query_async(&mut conn).await {
            Err(err) if dropped(&err) => {
                let mut conn = self.reconnect(db).await?;
                cmd.query_async(&mut conn).await.map_err(message)
            }
            outcome => outcome.map_err(message),
        }
    }

    async fn read_pipe<T: FromRedisValue>(&self, pipe: &Pipeline) -> Result<T, String> {
        let (mut conn, db) = self.conn().await;
        match pipe.query_async(&mut conn).await {
            Err(err) if dropped(&err) => {
                let mut conn = self.reconnect(db).await?;
                pipe.query_async(&mut conn).await.map_err(message)
            }
            outcome => outcome.map_err(message),
        }
    }

    /// Runs a change and records it in the query history. A dropped
    /// connection is replaced for next time, but the change isn't retried,
    /// since it may already have run.
    async fn write(&self, pipe: &Pipeline, commands: &[&Cmd]) -> Result<(), String> {
        let (mut conn, db) = self.conn().await;
        let started = Instant::now();
        let outcome = pipe.query_async::<Value>(&mut conn).await;
        if matches!(&outcome, Err(err) if dropped(err)) {
            let _ = self.reconnect(db).await;
        }
        let outcome = outcome.map(|_| ()).map_err(message);
        for cmd in commands {
            self.record(db, &describe(cmd), QueryOrigin::Edit, started, &outcome);
        }
        outcome
    }

    fn record(
        &self,
        db: u32,
        text: &str,
        origin: QueryOrigin,
        started: Instant,
        outcome: &Result<(), String>,
    ) {
        query_log::record(QueryRecord {
            connection_id: &self.entry.id,
            connection: &self.entry.name,
            driver: Driver::Redis.label(),
            database: &db.to_string(),
            sql: text,
            origin,
            duration: started.elapsed(),
            outcome: outcome.as_ref().map(|_| None).map_err(|err| err.as_str()),
        });
    }

    async fn close(&self) {
        if let Some(tunnel) = &self.tunnel {
            ssh::release(tunnel).await;
        }
    }
}

/// A key or value. `hex` holds the exact bytes when they aren't UTF-8 text,
/// and `text` is then only for display.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Bytes {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hex: Option<String>,
}

impl Bytes {
    fn from_vec(bytes: Vec<u8>) -> Self {
        match String::from_utf8(bytes) {
            Ok(text) => Bytes { text, hex: None },
            Err(err) => {
                let bytes = err.into_bytes();
                Bytes {
                    text: String::from_utf8_lossy(&bytes).into_owned(),
                    hex: Some(to_hex(&bytes)),
                }
            }
        }
    }

    fn to_vec(&self) -> Result<Vec<u8>, String> {
        match &self.hex {
            Some(hex) => from_hex(hex),
            None => Ok(self.text.as_bytes().to_vec()),
        }
    }
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn from_hex(hex: &str) -> Result<Vec<u8>, String> {
    let hex: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
    if !hex.len().is_multiple_of(2) {
        return Err("Hex values need two digits per byte.".into());
    }
    (0..hex.len())
        .step_by(2)
        .map(|at| {
            u8::from_str_radix(&hex[at..at + 2], 16)
                .map_err(|_| format!("{} isn't a hex byte.", &hex[at..at + 2]))
        })
        .collect()
}

/// A command as text for the query history, quoting arguments that need it.
fn describe(cmd: &Cmd) -> String {
    cmd.args_iter()
        .filter_map(|arg| match arg {
            redis::Arg::Simple(bytes) => Some(quote_arg(&String::from_utf8_lossy(bytes))),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote_arg(arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg
            .chars()
            .all(|c| !c.is_whitespace() && !matches!(c, '"' | '\'' | '\\'));
    if plain {
        return arg.to_string();
    }
    let escaped: String = arg
        .chars()
        .flat_map(|c| match c {
            '"' => vec!['\\', '"'],
            '\\' => vec!['\\', '\\'],
            '\n' => vec!['\\', 'n'],
            '\r' => vec!['\\', 'r'],
            '\t' => vec!['\\', 't'],
            c => vec![c],
        })
        .collect();
    format!("\"{escaped}\"")
}

/**
 * Splits a console line the way redis-cli does: whitespace between
 * arguments, double quotes with backslash escapes (including `\xHH`), and
 * single quotes taken literally.
 */
pub fn parse_args(line: &str) -> Result<Vec<Vec<u8>>, String> {
    let mut args = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let Some(&first) = chars.peek() else {
            return Ok(args);
        };
        let mut arg = Vec::new();
        let push = |arg: &mut Vec<u8>, c: char| {
            let mut buf = [0; 4];
            arg.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        };
        match first {
            '"' => {
                chars.next();
                loop {
                    match chars.next() {
                        None => return Err("A double quote isn't closed.".into()),
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some('n') => arg.push(b'\n'),
                            Some('r') => arg.push(b'\r'),
                            Some('t') => arg.push(b'\t'),
                            Some('b') => arg.push(8),
                            Some('a') => arg.push(7),
                            Some('x') => {
                                let hex: String = chars.by_ref().take(2).collect();
                                let byte = u8::from_str_radix(&hex, 16)
                                    .map_err(|_| format!("\\x{hex} isn't a hex byte."))?;
                                arg.push(byte);
                            }
                            Some(c) => push(&mut arg, c),
                            None => return Err("A double quote isn't closed.".into()),
                        },
                        Some(c) => push(&mut arg, c),
                    }
                }
            }
            '\'' => {
                chars.next();
                loop {
                    match chars.next() {
                        None => return Err("A single quote isn't closed.".into()),
                        Some('\\') if chars.peek() == Some(&'\'') => {
                            chars.next();
                            arg.push(b'\'');
                        }
                        Some('\'') => break,
                        Some(c) => push(&mut arg, c),
                    }
                }
            }
            _ => {
                while let Some(&c) = chars.peek() {
                    if c.is_whitespace() {
                        break;
                    }
                    push(&mut arg, c);
                    chars.next();
                }
            }
        }
        if chars.peek().is_some_and(|c| !c.is_whitespace()) {
            return Err("Put a space after a closing quote.".into());
        }
        args.push(arg);
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseInfo {
    index: u32,
    keys: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RedisInfo {
    server_version: String,
    db: u32,
    databases: Vec<DatabaseInfo>,
}

async fn databases(session: &RedisSession) -> Result<Vec<DatabaseInfo>, String> {
    // Managed services often disable CONFIG, so fall back to the usual 16.
    let configured: Option<u32> = session
        .read::<Vec<String>>(redis::cmd("CONFIG").arg("GET").arg("databases"))
        .await
        .ok()
        .and_then(|pair| pair.get(1).and_then(|count| count.parse().ok()));
    let keyspace: String = session.read(redis::cmd("INFO").arg("keyspace")).await?;
    let mut counts = HashMap::new();
    for (name, stats) in parse_info(&keyspace) {
        let Some(index) = name.strip_prefix("db").and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        let keys = stats
            .split(',')
            .find_map(|part| part.strip_prefix("keys="))
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        counts.insert(index, keys);
    }
    let highest = counts.keys().max().map(|n| n + 1).unwrap_or(0);
    let total = configured
        .unwrap_or(DEFAULT_DATABASES)
        .max(highest)
        .max(session.live.lock().await.db + 1);
    Ok((0..total)
        .map(|index| DatabaseInfo {
            index,
            keys: counts.get(&index).copied().unwrap_or(0),
        })
        .collect())
}

fn find_entry(state: &AppState, connection_id: &str) -> Result<ConnectionEntry, String> {
    let data = state.data.lock().map_err(|err| err.to_string())?;
    data.find_connection(connection_id)
        .cloned()
        .ok_or_else(|| "Connection not found".to_string())
}

#[tauri::command]
pub async fn redis_connect(
    app: AppHandle,
    state: State<'_, AppState>,
    store: State<'_, RedisStore>,
    connection_id: String,
    password: Option<String>,
    session_id: Option<String>,
    db: Option<u32>,
) -> Result<RedisInfo, String> {
    let entry = find_entry(&state, &connection_id)?;
    if entry.driver != Driver::Redis {
        return Err("This isn't a Redis connection.".into());
    }
    let key = session_id
        .filter(|id| !id.is_empty())
        .unwrap_or(connection_id);
    if let Some(previous) = store.0.lock().await.remove(&key) {
        previous.close().await;
    }
    let prompter = Prompter::interactive(&app, entry.name.clone());
    let password = password_for(&entry, password, &prompter).await?;
    let material = tls::load(&entry).await?;
    let (target, tunnel) = open_tunnel(&entry, None, None, &prompter).await?;
    let db = db.unwrap_or_else(|| entry.database.parse().unwrap_or(0));
    let opened = async {
        let client = client_for(&target, password.as_deref(), &material, db)?;
        let mut conn = open_connection(&client, RESPONSE_TIMEOUT).await?;
        let version = server_version(&mut conn).await?;
        Ok::<_, String>((conn, version))
    }
    .await;
    let (conn, version) = match opened {
        Ok(opened) => opened,
        Err(err) => {
            let err = explain(tunnel.as_deref(), err);
            if let Some(tunnel) = &tunnel {
                ssh::release(tunnel).await;
            }
            return Err(err);
        }
    };
    let session = Arc::new(RedisSession {
        entry,
        target,
        password,
        tls: material,
        tunnel,
        live: Mutex::new(Live {
            db,
            conn,
            console: None,
        }),
    });
    let databases = databases(&session).await.unwrap_or_default();
    if let Some(stale) = store.0.lock().await.insert(key, session) {
        stale.close().await;
    }
    Ok(RedisInfo {
        server_version: version,
        db,
        databases,
    })
}

#[tauri::command]
pub async fn redis_disconnect(
    store: State<'_, RedisStore>,
    session_id: String,
) -> Result<(), String> {
    if let Some(session) = store.0.lock().await.remove(&session_id) {
        session.close().await;
    }
    Ok(())
}

#[tauri::command]
pub async fn redis_databases(
    store: State<'_, RedisStore>,
    session_id: String,
) -> Result<Vec<DatabaseInfo>, String> {
    databases(&*store.get(&session_id).await?).await
}

#[tauri::command]
pub async fn redis_select_db(
    store: State<'_, RedisStore>,
    session_id: String,
    db: u32,
) -> Result<(), String> {
    let session = store.get(&session_id).await?;
    let conn = session.connect(db, RESPONSE_TIMEOUT).await?;
    let mut live = session.live.lock().await;
    live.db = db;
    live.conn = conn;
    live.console = None;
    Ok(())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyEntry {
    key: Bytes,
    kind: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanPage {
    /// Where the next page starts, or `None` once the whole keyspace was scanned.
    cursor: Option<String>,
    keys: Vec<KeyEntry>,
}

/**
 * One page of keys matching `pattern`. SCAN can return few or no matches
 * per call on a large keyspace, so this keeps scanning until it has `count`
 * keys, the scan ends, or `SCAN_BUDGET` passes.
 */
#[tauri::command]
pub async fn redis_scan(
    store: State<'_, RedisStore>,
    session_id: String,
    cursor: Option<String>,
    pattern: String,
    kind: Option<String>,
    count: u32,
) -> Result<ScanPage, String> {
    scan_page(
        &*store.get(&session_id).await?,
        cursor,
        pattern,
        kind,
        count,
    )
    .await
}

async fn scan_page(
    session: &RedisSession,
    cursor: Option<String>,
    pattern: String,
    kind: Option<String>,
    count: u32,
) -> Result<ScanPage, String> {
    let count = count.clamp(1, PAGE_MAX) as usize;
    let pattern = if pattern.trim().is_empty() {
        "*".to_string()
    } else {
        pattern
    };
    let kind = kind.filter(|kind| !kind.is_empty());
    let mut cursor = cursor.unwrap_or_else(|| "0".into());
    let mut found: Vec<Vec<u8>> = Vec::new();
    let started = Instant::now();
    loop {
        let mut cmd = redis::cmd("SCAN");
        cmd.arg(&cursor)
            .arg("MATCH")
            .arg(&pattern)
            .arg("COUNT")
            .arg((count as u32 * 4).min(SCAN_COUNT_MAX));
        if let Some(kind) = &kind {
            cmd.arg("TYPE").arg(kind);
        }
        let (next, keys): (String, Vec<Vec<u8>>) = session.read(&cmd).await?;
        found.extend(keys);
        cursor = next;
        if cursor == "0" || found.len() >= count || started.elapsed() > SCAN_BUDGET {
            break;
        }
    }
    let kinds: Vec<String> = if found.is_empty() {
        Vec::new()
    } else if let Some(kind) = &kind {
        vec![kind.clone(); found.len()]
    } else {
        let mut pipe = redis::pipe();
        for key in &found {
            pipe.cmd("TYPE").arg(key);
        }
        session.read_pipe(&pipe).await?
    };
    let keys = found
        .into_iter()
        .zip(kinds)
        .filter(|(_, kind)| kind != "none")
        .map(|(key, kind)| KeyEntry {
            key: Bytes::from_vec(key),
            kind,
        })
        .collect();
    Ok(ScanPage {
        cursor: (cursor != "0").then_some(cursor),
        keys,
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyInfo {
    kind: String,
    /// Milliseconds left, -1 for no expiry, or -2 once the key is gone.
    ttl_ms: i64,
    length: Option<u64>,
    memory: Option<u64>,
    encoding: Option<String>,
}

fn length_command(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "string" => "STRLEN",
        "hash" => "HLEN",
        "list" => "LLEN",
        "set" => "SCARD",
        "zset" => "ZCARD",
        "stream" => "XLEN",
        _ => return None,
    })
}

#[tauri::command]
pub async fn redis_key_info(
    store: State<'_, RedisStore>,
    session_id: String,
    key: Bytes,
) -> Result<KeyInfo, String> {
    key_info(&*store.get(&session_id).await?, key).await
}

async fn key_info(session: &RedisSession, key: Bytes) -> Result<KeyInfo, String> {
    let key = key.to_vec()?;
    let mut pipe = redis::pipe();
    pipe.cmd("TYPE").arg(&key).cmd("PTTL").arg(&key);
    let (kind, ttl_ms): (String, i64) = session.read_pipe(&pipe).await?;
    let length = match length_command(&kind) {
        Some(command) => session.read(redis::cmd(command).arg(&key)).await.ok(),
        None => None,
    };
    // Either can be disabled by ACLs or on managed services.
    let memory = session
        .read::<Option<u64>>(redis::cmd("MEMORY").arg("USAGE").arg(&key))
        .await
        .ok()
        .flatten();
    let encoding = session
        .read::<Option<String>>(redis::cmd("OBJECT").arg("ENCODING").arg(&key))
        .await
        .ok()
        .flatten();
    Ok(KeyInfo {
        kind,
        ttl_ms,
        length,
        memory,
        encoding,
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldValue {
    field: Bytes,
    value: Bytes,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListItem {
    index: i64,
    value: Bytes,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoredMember {
    member: Bytes,
    score: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamEntry {
    id: String,
    fields: Vec<FieldValue>,
}

/// One page of a key's value. `cursor` is where the next page starts, or
/// `None` on the last page.
#[derive(Debug, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "lowercase",
    rename_all_fields = "camelCase"
)]
pub enum ValuePage {
    String {
        value: Bytes,
        size: u64,
        truncated: bool,
    },
    Hash {
        entries: Vec<FieldValue>,
        cursor: Option<String>,
    },
    List {
        items: Vec<ListItem>,
        cursor: Option<String>,
    },
    Set {
        members: Vec<Bytes>,
        cursor: Option<String>,
    },
    Zset {
        members: Vec<ScoredMember>,
        cursor: Option<String>,
    },
    Stream {
        entries: Vec<StreamEntry>,
        cursor: Option<String>,
    },
    Other {
        type_name: String,
    },
}

fn scan_cursor(next: String) -> Option<String> {
    (next != "0").then_some(next)
}

fn pairs(flat: Vec<Vec<u8>>) -> Vec<FieldValue> {
    let mut items = flat.into_iter();
    let mut out = Vec::new();
    while let (Some(field), Some(value)) = (items.next(), items.next()) {
        out.push(FieldValue {
            field: Bytes::from_vec(field),
            value: Bytes::from_vec(value),
        });
    }
    out
}

fn index_cursor(cursor: Option<String>) -> Result<i64, String> {
    match cursor {
        None => Ok(0),
        Some(cursor) => cursor
            .parse()
            .map_err(|_| format!("{cursor} isn't a list position.")),
    }
}

#[tauri::command]
pub async fn redis_read(
    store: State<'_, RedisStore>,
    session_id: String,
    key: Bytes,
    kind: String,
    cursor: Option<String>,
    count: u32,
) -> Result<ValuePage, String> {
    read_page(&*store.get(&session_id).await?, key, kind, cursor, count).await
}

async fn read_page(
    session: &RedisSession,
    key: Bytes,
    kind: String,
    cursor: Option<String>,
    count: u32,
) -> Result<ValuePage, String> {
    let key = key.to_vec()?;
    let count = count.clamp(1, PAGE_MAX);
    Ok(match kind.as_str() {
        "string" => {
            let mut pipe = redis::pipe();
            pipe.cmd("STRLEN")
                .arg(&key)
                .cmd("GETRANGE")
                .arg(&key)
                .arg(0)
                .arg(STRING_PREVIEW as i64 - 1);
            let (size, value): (u64, Vec<u8>) = session.read_pipe(&pipe).await?;
            ValuePage::String {
                truncated: size > value.len() as u64,
                value: Bytes::from_vec(value),
                size,
            }
        }
        "hash" => {
            let (next, flat): (String, Vec<Vec<u8>>) = session
                .read(
                    redis::cmd("HSCAN")
                        .arg(&key)
                        .arg(cursor.as_deref().unwrap_or("0"))
                        .arg("COUNT")
                        .arg(count),
                )
                .await?;
            ValuePage::Hash {
                entries: pairs(flat),
                cursor: scan_cursor(next),
            }
        }
        "set" => {
            let (next, members): (String, Vec<Vec<u8>>) = session
                .read(
                    redis::cmd("SSCAN")
                        .arg(&key)
                        .arg(cursor.as_deref().unwrap_or("0"))
                        .arg("COUNT")
                        .arg(count),
                )
                .await?;
            ValuePage::Set {
                members: members.into_iter().map(Bytes::from_vec).collect(),
                cursor: scan_cursor(next),
            }
        }
        "list" => {
            let start = index_cursor(cursor)?;
            let stop = start + i64::from(count) - 1;
            let mut pipe = redis::pipe();
            pipe.cmd("LRANGE")
                .arg(&key)
                .arg(start)
                .arg(stop)
                .cmd("LLEN")
                .arg(&key);
            let (values, total): (Vec<Vec<u8>>, i64) = session.read_pipe(&pipe).await?;
            ValuePage::List {
                items: values
                    .into_iter()
                    .enumerate()
                    .map(|(at, value)| ListItem {
                        index: start + at as i64,
                        value: Bytes::from_vec(value),
                    })
                    .collect(),
                cursor: (stop + 1 < total).then(|| (stop + 1).to_string()),
            }
        }
        "zset" => {
            let start = index_cursor(cursor)?;
            let stop = start + i64::from(count) - 1;
            let mut pipe = redis::pipe();
            pipe.cmd("ZRANGE")
                .arg(&key)
                .arg(start)
                .arg(stop)
                .arg("WITHSCORES")
                .cmd("ZCARD")
                .arg(&key);
            let (flat, total): (Vec<Vec<u8>>, i64) = session.read_pipe(&pipe).await?;
            let mut members = Vec::new();
            let mut items = flat.into_iter();
            while let (Some(member), Some(score)) = (items.next(), items.next()) {
                let score = String::from_utf8_lossy(&score);
                members.push(ScoredMember {
                    member: Bytes::from_vec(member),
                    score: score.parse().unwrap_or(f64::NAN),
                });
            }
            ValuePage::Zset {
                members,
                cursor: (stop + 1 < total).then(|| (stop + 1).to_string()),
            }
        }
        "stream" => {
            // Newest first. The cursor is the last id shown, excluded from the next page.
            let end = match &cursor {
                Some(id) => format!("({id}"),
                None => "+".into(),
            };
            let raw: Vec<(String, Vec<Vec<u8>>)> = session
                .read(
                    redis::cmd("XREVRANGE")
                        .arg(&key)
                        .arg(end)
                        .arg("-")
                        .arg("COUNT")
                        .arg(count),
                )
                .await?;
            let full = raw.len() == count as usize;
            let next = raw.last().map(|(id, _)| id.clone());
            ValuePage::Stream {
                entries: raw
                    .into_iter()
                    .map(|(id, flat)| StreamEntry {
                        id,
                        fields: pairs(flat),
                    })
                    .collect(),
                cursor: next.filter(|_| full),
            }
        }
        _ => ValuePage::Other { type_name: kind },
    })
}

/// A change to one key, made from the key browser.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Edit {
    SetString {
        key: Bytes,
        value: Bytes,
    },
    HashSet {
        key: Bytes,
        field: Bytes,
        value: Bytes,
        /// The field being edited, when it was renamed.
        original: Option<Bytes>,
    },
    HashDelete {
        key: Bytes,
        fields: Vec<Bytes>,
    },
    ListSet {
        key: Bytes,
        index: i64,
        value: Bytes,
    },
    ListPush {
        key: Bytes,
        value: Bytes,
        head: bool,
    },
    /// Indexes as they were before any change in the same batch.
    ListRemove {
        key: Bytes,
        indexes: Vec<i64>,
    },
    SetAdd {
        key: Bytes,
        member: Bytes,
        /// The member being edited, which is removed.
        original: Option<Bytes>,
    },
    SetRemove {
        key: Bytes,
        members: Vec<Bytes>,
    },
    ZsetAdd {
        key: Bytes,
        member: Bytes,
        score: f64,
        original: Option<Bytes>,
    },
    ZsetRemove {
        key: Bytes,
        members: Vec<Bytes>,
    },
    StreamAdd {
        key: Bytes,
        fields: Vec<(Bytes, Bytes)>,
    },
    StreamDelete {
        key: Bytes,
        ids: Vec<String>,
    },
    Rename {
        key: Bytes,
        to: Bytes,
        overwrite: bool,
    },
    Expire {
        key: Bytes,
        /// Seconds from now, or `None` to keep the key forever.
        seconds: Option<u64>,
    },
    Delete {
        keys: Vec<Bytes>,
    },
}

fn many(items: &[Bytes]) -> Result<Vec<Vec<u8>>, String> {
    items.iter().map(Bytes::to_vec).collect()
}

/// Marks a list element for removal, since Redis can only remove list elements by value.
const LIST_TOMBSTONE: &str = "__recon_removed__";

fn edit_commands(edit: Edit) -> Result<Vec<Cmd>, String> {
    let mut cmds = Vec::new();
    match edit {
        Edit::SetString { key, value } => {
            cmds.push(
                redis::cmd("SET")
                    .arg(key.to_vec()?)
                    .arg(value.to_vec()?)
                    .arg("KEEPTTL")
                    .to_owned(),
            );
        }
        Edit::HashSet {
            key,
            field,
            value,
            original,
        } => {
            let key = key.to_vec()?;
            let field = field.to_vec()?;
            if let Some(original) = original.map(|o| o.to_vec()).transpose()? {
                if original != field {
                    cmds.push(redis::cmd("HDEL").arg(&key).arg(original).to_owned());
                }
            }
            cmds.push(
                redis::cmd("HSET")
                    .arg(&key)
                    .arg(field)
                    .arg(value.to_vec()?)
                    .to_owned(),
            );
        }
        Edit::HashDelete { key, fields } => {
            cmds.push(
                redis::cmd("HDEL")
                    .arg(key.to_vec()?)
                    .arg(many(&fields)?)
                    .to_owned(),
            );
        }
        Edit::ListSet { key, index, value } => {
            cmds.push(
                redis::cmd("LSET")
                    .arg(key.to_vec()?)
                    .arg(index)
                    .arg(value.to_vec()?)
                    .to_owned(),
            );
        }
        Edit::ListPush { key, value, head } => {
            let command = if head { "LPUSH" } else { "RPUSH" };
            cmds.push(
                redis::cmd(command)
                    .arg(key.to_vec()?)
                    .arg(value.to_vec()?)
                    .to_owned(),
            );
        }
        Edit::ListRemove { key, indexes } => {
            if indexes.is_empty() {
                return Ok(cmds);
            }
            let key = key.to_vec()?;
            for index in indexes {
                cmds.push(
                    redis::cmd("LSET")
                        .arg(&key)
                        .arg(index)
                        .arg(LIST_TOMBSTONE)
                        .to_owned(),
                );
            }
            cmds.push(
                redis::cmd("LREM")
                    .arg(&key)
                    .arg(0)
                    .arg(LIST_TOMBSTONE)
                    .to_owned(),
            );
        }
        Edit::SetAdd {
            key,
            member,
            original,
        } => {
            let key = key.to_vec()?;
            let member = member.to_vec()?;
            if let Some(original) = original.map(|o| o.to_vec()).transpose()? {
                if original != member {
                    cmds.push(redis::cmd("SREM").arg(&key).arg(original).to_owned());
                }
            }
            cmds.push(redis::cmd("SADD").arg(&key).arg(member).to_owned());
        }
        Edit::SetRemove { key, members } => {
            cmds.push(
                redis::cmd("SREM")
                    .arg(key.to_vec()?)
                    .arg(many(&members)?)
                    .to_owned(),
            );
        }
        Edit::ZsetAdd {
            key,
            member,
            score,
            original,
        } => {
            if !score.is_finite() {
                return Err("A score must be a number.".into());
            }
            let key = key.to_vec()?;
            let member = member.to_vec()?;
            if let Some(original) = original.map(|o| o.to_vec()).transpose()? {
                if original != member {
                    cmds.push(redis::cmd("ZREM").arg(&key).arg(original).to_owned());
                }
            }
            cmds.push(
                redis::cmd("ZADD")
                    .arg(&key)
                    .arg(score)
                    .arg(member)
                    .to_owned(),
            );
        }
        Edit::ZsetRemove { key, members } => {
            cmds.push(
                redis::cmd("ZREM")
                    .arg(key.to_vec()?)
                    .arg(many(&members)?)
                    .to_owned(),
            );
        }
        Edit::StreamAdd { key, fields } => {
            if fields.is_empty() {
                return Err("A stream entry needs at least one field.".into());
            }
            let mut cmd = redis::cmd("XADD");
            cmd.arg(key.to_vec()?).arg("*");
            for (field, value) in &fields {
                cmd.arg(field.to_vec()?).arg(value.to_vec()?);
            }
            cmds.push(cmd);
        }
        Edit::StreamDelete { key, ids } => {
            cmds.push(redis::cmd("XDEL").arg(key.to_vec()?).arg(ids).to_owned());
        }
        Edit::Rename { key, to, overwrite } => {
            let command = if overwrite { "RENAME" } else { "RENAMENX" };
            cmds.push(
                redis::cmd(command)
                    .arg(key.to_vec()?)
                    .arg(to.to_vec()?)
                    .to_owned(),
            );
        }
        Edit::Expire { key, seconds } => {
            cmds.push(match seconds {
                Some(seconds) => redis::cmd("EXPIRE")
                    .arg(key.to_vec()?)
                    .arg(seconds.max(1))
                    .to_owned(),
                None => redis::cmd("PERSIST").arg(key.to_vec()?).to_owned(),
            });
        }
        Edit::Delete { keys } => {
            if keys.is_empty() {
                return Ok(cmds);
            }
            cmds.push(redis::cmd("UNLINK").arg(many(&keys)?).to_owned());
        }
    }
    Ok(cmds)
}

#[tauri::command]
pub async fn redis_edit(
    store: State<'_, RedisStore>,
    session_id: String,
    edit: Edit,
) -> Result<(), String> {
    apply_edit(&*store.get(&session_id).await?, edit).await
}

async fn apply_edit(session: &RedisSession, edit: Edit) -> Result<(), String> {
    let renames_without_overwrite = matches!(
        &edit,
        Edit::Rename {
            overwrite: false,
            ..
        }
    );
    let cmds = edit_commands(edit)?;
    if cmds.is_empty() {
        return Ok(());
    }
    if renames_without_overwrite {
        let (mut conn, db) = session.conn().await;
        let started = Instant::now();
        let renamed: Result<bool, String> = cmds[0].query_async(&mut conn).await.map_err(message);
        let outcome = renamed.as_ref().map(|_| ()).map_err(Clone::clone);
        session.record(
            db,
            &describe(&cmds[0]),
            QueryOrigin::Edit,
            started,
            &outcome,
        );
        return match renamed? {
            true => Ok(()),
            false => Err("A key with that name already exists.".into()),
        };
    }
    let mut pipe = redis::pipe();
    pipe.atomic();
    for cmd in &cmds {
        pipe.add_command(cmd.clone());
    }
    let refs: Vec<&Cmd> = cmds.iter().collect();
    session.write(&pipe, &refs).await
}

/**
 * Saves a grid's pending changes in one MULTI/EXEC, so they all apply or
 * none do. List indexes refer to the list as loaded, so callers send list
 * sets before list removals, and pushes last.
 */
#[tauri::command]
pub async fn redis_edit_batch(
    store: State<'_, RedisStore>,
    session_id: String,
    edits: Vec<Edit>,
) -> Result<(), String> {
    apply_batch(&*store.get(&session_id).await?, edits).await
}

async fn apply_batch(session: &RedisSession, edits: Vec<Edit>) -> Result<(), String> {
    if edits.iter().any(|edit| matches!(edit, Edit::Rename { .. })) {
        return Err("Rename keys one at a time.".into());
    }
    let mut cmds = Vec::new();
    for edit in edits {
        cmds.extend(edit_commands(edit)?);
    }
    if cmds.is_empty() {
        return Ok(());
    }
    let mut pipe = redis::pipe();
    pipe.atomic();
    for cmd in &cmds {
        pipe.add_command(cmd.clone());
    }
    let refs: Vec<&Cmd> = cmds.iter().collect();
    session.write(&pipe, &refs).await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewKey {
    key: Bytes,
    kind: String,
    /// The hash field, or the sorted set member.
    field: Option<Bytes>,
    value: Bytes,
    score: Option<f64>,
    ttl_seconds: Option<u64>,
}

#[tauri::command]
pub async fn redis_create_key(
    store: State<'_, RedisStore>,
    session_id: String,
    key: NewKey,
) -> Result<(), String> {
    create_key(&*store.get(&session_id).await?, key).await
}

async fn create_key(session: &RedisSession, key: NewKey) -> Result<(), String> {
    let name = key.key.to_vec()?;
    if name.is_empty() {
        return Err("A key needs a name.".into());
    }
    let exists: bool = session.read(redis::cmd("EXISTS").arg(&name)).await?;
    if exists {
        return Err("A key with that name already exists.".into());
    }
    let value = key.value.to_vec()?;
    let field = || {
        key.field
            .as_ref()
            .map(Bytes::to_vec)
            .transpose()
            .map(Option::unwrap_or_default)
    };
    let create = match key.kind.as_str() {
        "string" => redis::cmd("SET").arg(&name).arg(&value).to_owned(),
        "hash" => redis::cmd("HSET")
            .arg(&name)
            .arg(field()?)
            .arg(&value)
            .to_owned(),
        "list" => redis::cmd("RPUSH").arg(&name).arg(&value).to_owned(),
        "set" => redis::cmd("SADD").arg(&name).arg(&value).to_owned(),
        "zset" => {
            let score = key.score.unwrap_or(0.0);
            if !score.is_finite() {
                return Err("A score must be a number.".into());
            }
            redis::cmd("ZADD")
                .arg(&name)
                .arg(score)
                .arg(&value)
                .to_owned()
        }
        "stream" => redis::cmd("XADD")
            .arg(&name)
            .arg("*")
            .arg(field()?)
            .arg(&value)
            .to_owned(),
        other => return Err(format!("Recon can't create {other} keys.")),
    };
    let mut cmds = vec![create];
    if let Some(seconds) = key.ttl_seconds.filter(|s| *s > 0) {
        cmds.push(redis::cmd("EXPIRE").arg(&name).arg(seconds).to_owned());
    }
    let mut pipe = redis::pipe();
    pipe.atomic();
    for cmd in &cmds {
        pipe.add_command(cmd.clone());
    }
    let refs: Vec<&Cmd> = cmds.iter().collect();
    session.write(&pipe, &refs).await
}

/// A console reply, shaped like redis-cli shows it.
#[derive(Debug, Serialize)]
#[serde(tag = "type", content = "value", rename_all = "lowercase")]
pub enum Reply {
    Nil,
    Int(i64),
    Bulk(Bytes),
    Status(String),
    Error(String),
    Array(Vec<Reply>),
    Set(Vec<Reply>),
    Map(Vec<(Reply, Reply)>),
    Double(f64),
    Bool(bool),
    Big(String),
}

fn reply(value: Value) -> Reply {
    match value {
        Value::Nil => Reply::Nil,
        Value::Int(n) => Reply::Int(n),
        Value::BulkString(bytes) => Reply::Bulk(Bytes::from_vec(bytes)),
        Value::SimpleString(text) => Reply::Status(text),
        Value::Okay => Reply::Status("OK".into()),
        Value::Array(items) => Reply::Array(items.into_iter().map(reply).collect()),
        Value::Set(items) => Reply::Set(items.into_iter().map(reply).collect()),
        Value::Map(pairs) => Reply::Map(
            pairs
                .into_iter()
                .map(|(key, value)| (reply(key), reply(value)))
                .collect(),
        ),
        Value::Attribute { data, .. } => reply(*data),
        Value::Double(n) => Reply::Double(n),
        Value::Boolean(b) => Reply::Bool(b),
        Value::VerbatimString { text, .. } => Reply::Bulk(Bytes { text, hex: None }),
        Value::BigNumber(n) => Reply::Big(format!("{n:?}")),
        Value::Push { data, .. } => Reply::Array(data.into_iter().map(reply).collect()),
        Value::ServerError(err) => Reply::Error(err.to_string()),
        other => Reply::Error(format!("Unsupported reply: {other:?}")),
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandResult {
    reply: Reply,
    duration_ms: u64,
}

#[tauri::command]
pub async fn redis_command(
    store: State<'_, RedisStore>,
    session_id: String,
    line: String,
) -> Result<CommandResult, String> {
    run_command(&*store.get(&session_id).await?, line).await
}

async fn run_command(session: &RedisSession, line: String) -> Result<CommandResult, String> {
    let args = parse_args(&line)?;
    let Some(name) = args.first() else {
        return Err("Type a command.".into());
    };
    let name = String::from_utf8_lossy(name).to_ascii_uppercase();
    if let Some((_, why)) = REFUSED.iter().find(|(refused, _)| *refused == name) {
        return Err((*why).to_string());
    }
    let mut cmd = redis::cmd(&name);
    for arg in &args[1..] {
        cmd.arg(arg);
    }
    let (conn, db) = {
        let mut live = session.live.lock().await;
        let db = live.db;
        let conn = match &live.console {
            Some(conn) => conn.clone(),
            None => {
                let conn = session.connect(db, CONSOLE_TIMEOUT).await?;
                live.console = Some(conn.clone());
                conn
            }
        };
        (conn, db)
    };
    let mut conn = conn;
    let started = Instant::now();
    let outcome = conn.req_packed_command(&cmd).await;
    let duration = started.elapsed();
    let reply = match outcome {
        Ok(value) => reply(value),
        Err(err) => {
            if dropped(&err) || err.is_timeout() {
                // A blocked or broken console connection is replaced next time.
                session.live.lock().await.console = None;
            }
            Reply::Error(message(err))
        }
    };
    let logged = match &reply {
        Reply::Error(err) => Err(err.clone()),
        _ => Ok(()),
    };
    session.record(db, line.trim(), QueryOrigin::Editor, started, &logged);
    Ok(CommandResult {
        reply,
        duration_ms: u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        parse_args(line)
            .unwrap()
            .into_iter()
            .map(|arg| String::from_utf8(arg).unwrap())
            .collect()
    }

    #[test]
    fn splits_like_redis_cli() {
        assert_eq!(args("  SET  key value "), ["SET", "key", "value"]);
        assert_eq!(
            args(r#"SET "a key" "line\nbreak""#),
            ["SET", "a key", "line\nbreak"]
        );
        assert_eq!(
            args(r"SET k 'it\'s literal \n'"),
            ["SET", "k", r"it's literal \n"]
        );
        assert_eq!(args(r#"SET k "\x41\x42""#), ["SET", "k", "AB"]);
        assert_eq!(args(r#"SET k """#), ["SET", "k", ""]);
        assert!(args("").is_empty());
    }

    #[test]
    fn rejects_broken_quotes() {
        assert!(parse_args(r#"SET "open"#).is_err());
        assert!(parse_args("SET 'open").is_err());
        assert!(parse_args(r#"SET "a"b"#).is_err());
    }

    #[test]
    fn bytes_round_trip() {
        let text = Bytes::from_vec(b"plain".to_vec());
        assert_eq!(text.hex, None);
        assert_eq!(text.to_vec().unwrap(), b"plain");
        let binary = Bytes::from_vec(vec![0xff, 0x00, 0x41]);
        assert_eq!(binary.hex.as_deref(), Some("ff0041"));
        assert_eq!(binary.to_vec().unwrap(), vec![0xff, 0x00, 0x41]);
    }

    #[test]
    fn describes_commands_for_history() {
        let cmd = redis::cmd("SET")
            .arg("a key")
            .arg("v\"1")
            .arg("plain")
            .to_owned();
        assert_eq!(describe(&cmd), r#"SET "a key" "v\"1" plain"#);
    }

    /// Runs against a real server: `REDIS_TEST_URL=127.0.0.1:6379 cargo test redis -- --ignored`.
    /// Uses database 15 and keys under a random prefix, which it deletes.
    #[tokio::test]
    #[ignore]
    async fn round_trips_against_a_server() {
        let address = std::env::var("REDIS_TEST_URL").unwrap_or_else(|_| "127.0.0.1:6379".into());
        let (host, port) = address.split_once(':').unwrap();
        let entry: ConnectionEntry = serde_json::from_value(serde_json::json!({
            "name": "test", "driver": "redis", "host": host, "port": port.parse::<u16>().unwrap(),
            "database": "15", "sslMode": "disable"
        }))
        .unwrap();
        let tls = TlsMaterial::default();
        let client = client_for(&entry, None, &tls, 15).unwrap();
        let conn = open_connection(&client, RESPONSE_TIMEOUT).await.unwrap();
        let session = RedisSession {
            entry: entry.clone(),
            target: entry,
            password: None,
            tls,
            tunnel: None,
            live: Mutex::new(Live {
                db: 15,
                conn,
                console: None,
            }),
        };
        let prefix = format!("recon-test:{}:", uuid::Uuid::new_v4());
        let name = |suffix: &str| Bytes {
            text: format!("{prefix}{suffix}"),
            hex: None,
        };
        let text = |value: &str| Bytes {
            text: value.into(),
            hex: None,
        };

        for (suffix, kind, field) in [
            ("s", "string", None),
            ("h", "hash", Some("f1")),
            ("l", "list", None),
            ("set", "set", None),
            ("z", "zset", None),
            ("x", "stream", Some("event")),
        ] {
            create_key(
                &session,
                NewKey {
                    key: name(suffix),
                    kind: kind.into(),
                    field: field.map(text),
                    value: text("one"),
                    score: Some(1.0),
                    ttl_seconds: None,
                },
            )
            .await
            .unwrap();
        }
        let duplicate = create_key(
            &session,
            NewKey {
                key: name("s"),
                kind: "string".into(),
                field: None,
                value: text("x"),
                score: None,
                ttl_seconds: None,
            },
        )
        .await;
        assert!(duplicate.is_err());

        let page = scan_page(&session, None, format!("{prefix}*"), None, 100)
            .await
            .unwrap();
        let mut kinds: Vec<_> = page.keys.iter().map(|k| k.kind.as_str()).collect();
        kinds.sort();
        assert_eq!(kinds, ["hash", "list", "set", "stream", "string", "zset"]);
        let only_hashes = scan_page(
            &session,
            None,
            format!("{prefix}*"),
            Some("hash".into()),
            100,
        )
        .await
        .unwrap();
        assert_eq!(only_hashes.keys.len(), 1);

        // Hash: add, rename a field, delete.
        apply_edit(
            &session,
            Edit::HashSet {
                key: name("h"),
                field: text("f2"),
                value: text("two"),
                original: None,
            },
        )
        .await
        .unwrap();
        apply_edit(
            &session,
            Edit::HashSet {
                key: name("h"),
                field: text("f3"),
                value: text("2!"),
                original: Some(text("f2")),
            },
        )
        .await
        .unwrap();
        let ValuePage::Hash { entries, .. } =
            read_page(&session, name("h"), "hash".into(), None, 100)
                .await
                .unwrap()
        else {
            panic!()
        };
        let mut fields: Vec<_> = entries.iter().map(|e| e.field.text.as_str()).collect();
        fields.sort();
        assert_eq!(fields, ["f1", "f3"]);
        apply_edit(
            &session,
            Edit::HashDelete {
                key: name("h"),
                fields: vec![text("f1")],
            },
        )
        .await
        .unwrap();
        assert_eq!(key_info(&session, name("h")).await.unwrap().length, Some(1));

        // List: push both ends, set, remove by index, page through.
        apply_edit(
            &session,
            Edit::ListPush {
                key: name("l"),
                value: text("zero"),
                head: true,
            },
        )
        .await
        .unwrap();
        apply_edit(
            &session,
            Edit::ListPush {
                key: name("l"),
                value: text("two"),
                head: false,
            },
        )
        .await
        .unwrap();
        apply_edit(
            &session,
            Edit::ListSet {
                key: name("l"),
                index: 2,
                value: text("TWO"),
            },
        )
        .await
        .unwrap();
        apply_batch(
            &session,
            vec![
                Edit::ListSet {
                    key: name("l"),
                    index: 0,
                    value: text("ZERO"),
                },
                Edit::ListRemove {
                    key: name("l"),
                    indexes: vec![1],
                },
                Edit::ListPush {
                    key: name("l"),
                    value: text("three"),
                    head: false,
                },
            ],
        )
        .await
        .unwrap();
        let ValuePage::List { items, cursor } =
            read_page(&session, name("l"), "list".into(), None, 1)
                .await
                .unwrap()
        else {
            panic!()
        };
        assert_eq!(items[0].value.text, "ZERO");
        assert_eq!(cursor.as_deref(), Some("1"));
        let ValuePage::List { items, cursor } =
            read_page(&session, name("l"), "list".into(), cursor, 1)
                .await
                .unwrap()
        else {
            panic!()
        };
        assert_eq!(
            (items[0].index, items[0].value.text.as_str(), cursor),
            (1, "TWO", Some("2".to_string()))
        );

        // Set and sorted set: replace a member.
        apply_edit(
            &session,
            Edit::SetAdd {
                key: name("set"),
                member: text("uno"),
                original: Some(text("one")),
            },
        )
        .await
        .unwrap();
        let ValuePage::Set { members, .. } =
            read_page(&session, name("set"), "set".into(), None, 10)
                .await
                .unwrap()
        else {
            panic!()
        };
        assert_eq!(members, vec![text("uno")]);
        apply_edit(
            &session,
            Edit::ZsetAdd {
                key: name("z"),
                member: text("top"),
                score: 9.5,
                original: None,
            },
        )
        .await
        .unwrap();
        apply_edit(
            &session,
            Edit::ZsetAdd {
                key: name("z"),
                member: text("first"),
                score: -1.0,
                original: Some(text("one")),
            },
        )
        .await
        .unwrap();
        let ValuePage::Zset { members, .. } =
            read_page(&session, name("z"), "zset".into(), None, 10)
                .await
                .unwrap()
        else {
            panic!()
        };
        let scored: Vec<_> = members
            .iter()
            .map(|m| (m.member.text.as_str(), m.score))
            .collect();
        assert_eq!(scored, [("first", -1.0), ("top", 9.5)]);

        // Stream: newest first, then delete.
        apply_edit(
            &session,
            Edit::StreamAdd {
                key: name("x"),
                fields: vec![(text("event"), text("two"))],
            },
        )
        .await
        .unwrap();
        let ValuePage::Stream { entries, .. } =
            read_page(&session, name("x"), "stream".into(), None, 10)
                .await
                .unwrap()
        else {
            panic!()
        };
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].fields[0].value.text, "two");
        apply_edit(
            &session,
            Edit::StreamDelete {
                key: name("x"),
                ids: vec![entries[0].id.clone()],
            },
        )
        .await
        .unwrap();
        assert_eq!(key_info(&session, name("x")).await.unwrap().length, Some(1));

        // String: binary bytes survive, KEEPTTL keeps the expiry.
        apply_edit(
            &session,
            Edit::Expire {
                key: name("s"),
                seconds: Some(300),
            },
        )
        .await
        .unwrap();
        apply_edit(
            &session,
            Edit::SetString {
                key: name("s"),
                value: Bytes {
                    text: String::new(),
                    hex: Some("ff00".into()),
                },
            },
        )
        .await
        .unwrap();
        let ValuePage::String {
            value,
            size,
            truncated,
        } = read_page(&session, name("s"), "string".into(), None, 1)
            .await
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(
            (value.hex.as_deref(), size, truncated),
            (Some("ff00"), 2, false)
        );
        assert!(key_info(&session, name("s")).await.unwrap().ttl_ms > 0);
        apply_edit(
            &session,
            Edit::Expire {
                key: name("s"),
                seconds: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(key_info(&session, name("s")).await.unwrap().ttl_ms, -1);

        // Rename refuses to overwrite unless asked.
        let clash = apply_edit(
            &session,
            Edit::Rename {
                key: name("s"),
                to: name("h"),
                overwrite: false,
            },
        )
        .await;
        assert_eq!(clash.unwrap_err(), "A key with that name already exists.");
        apply_edit(
            &session,
            Edit::Rename {
                key: name("s"),
                to: name("s2"),
                overwrite: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(key_info(&session, name("s2")).await.unwrap().kind, "string");

        // Console.
        let result = run_command(&session, format!("HGETALL {prefix}h"))
            .await
            .unwrap();
        assert!(matches!(result.reply, Reply::Array(ref items) if items.len() == 2));
        let result = run_command(&session, "NOSUCHCOMMAND".into()).await.unwrap();
        assert!(matches!(result.reply, Reply::Error(_)));
        assert!(run_command(&session, "select 1".into()).await.is_err());
        let result = run_command(&session, format!("EXISTS {prefix}gone"))
            .await
            .unwrap();
        assert!(matches!(result.reply, Reply::Int(0)));

        let page = scan_page(&session, None, format!("{prefix}*"), None, 100)
            .await
            .unwrap();
        let keys = page.keys.into_iter().map(|k| k.key).collect();
        apply_edit(&session, Edit::Delete { keys }).await.unwrap();
        let page = scan_page(&session, None, format!("{prefix}*"), None, 100)
            .await
            .unwrap();
        assert!(page.keys.is_empty());
    }

    #[test]
    fn parses_info_sections() {
        let info = parse_info("# Keyspace\r\ndb0:keys=3,expires=0\r\ndb2:keys=10,expires=1\r\n");
        assert_eq!(info.get("db0").unwrap(), "keys=3,expires=0");
        assert_eq!(info.len(), 2);
    }
}
