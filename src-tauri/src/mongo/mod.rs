//! MongoDB connections: a database and collection browser, document
//! editing, indexes, and a console.
//!
//! Like Redis, MongoDB has no tables or SQL, so it keeps its own sessions
//! here instead of going through the SQL `Session` in `db`. It shares the
//! connection settings, passwords, TLS material, and SSH tunnels.

mod console;
pub mod literal;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{future::join_all, TryStreamExt};
use mongodb::bson::{doc, Bson, Document};
use mongodb::error::{ErrorKind, WriteFailure};
use mongodb::options::{ClientOptions, Credential, ServerAddress, Tls, TlsOptions};
use mongodb::results::CollectionType;
use mongodb::{Client, Collection, Database};
use serde::Serialize;
use tauri::{AppHandle, State};
use tokio::sync::Mutex;

use crate::commands::AppState;
use crate::db::ssh::{self, Tunnel};
use crate::db::tls::{self, TlsMaterial};
use crate::db_commands::{explain, open_tunnel, password_for};
use crate::models::{ConnectionEntry, Driver};
use crate::prompts::Prompter;
use crate::query_log::{self, QueryOrigin, QueryRecord};

use console::Command;
use literal::{document_text, parse_document, summary, to_json, to_text};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a document page may take before the server stops it.
const QUERY_TIMEOUT: Duration = Duration::from_secs(30);
/// How long counting a filter's matches may take. Past this the count is left out.
const COUNT_TIMEOUT: Duration = Duration::from_secs(3);
const PAGE_MAX: i64 = 5_000;
/// What a console find shows unless it says .limit(n).
const CONSOLE_LIMIT: i64 = 50;
const CONSOLE_MAX: i64 = 1_000;
const SYSTEM_DATABASES: &[&str] = &["admin", "config", "local"];

#[derive(Default)]
pub struct MongoStore(Mutex<HashMap<String, Arc<MongoSession>>>);

impl MongoStore {
    async fn get(&self, id: &str) -> Result<Arc<MongoSession>, String> {
        self.0
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| "This connection is closed.".to_string())
    }
}

pub struct MongoSession {
    entry: ConnectionEntry,
    client: Client,
    tunnel: Option<Arc<Tunnel>>,
}

impl MongoSession {
    fn database(&self, name: &str) -> Result<Database, String> {
        check_database_name(name)?;
        if name.is_empty() {
            return Err("Choose a database first.".into());
        }
        Ok(self.client.database(name))
    }

    fn collection(&self, db: &str, name: &str) -> Result<Collection<Document>, String> {
        if name.is_empty() {
            return Err("Choose a collection first.".into());
        }
        Ok(self.database(db)?.collection(name))
    }

    fn explain(&self, err: mongodb::error::Error) -> String {
        explain(self.tunnel.as_deref(), message(err))
    }

    fn record<T>(
        &self,
        db: &str,
        text: &str,
        origin: QueryOrigin,
        started: Instant,
        outcome: &Result<T, String>,
    ) {
        query_log::record(QueryRecord {
            connection_id: &self.entry.id,
            connection: &self.entry.name,
            driver: Driver::Mongo.label(),
            database: db,
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

/// Error text without the driver's labels and topology dumps.
fn message(err: mongodb::error::Error) -> String {
    match err.kind.as_ref() {
        ErrorKind::Command(command) => command.message.clone(),
        ErrorKind::Write(WriteFailure::WriteError(write)) => write.message.clone(),
        ErrorKind::Write(WriteFailure::WriteConcernError(concern)) => concern.message.clone(),
        ErrorKind::InsertMany(failure) => failure
            .write_errors
            .as_ref()
            .and_then(|errors| errors.first())
            .map(|first| first.message.clone())
            .unwrap_or_else(|| err.to_string()),
        ErrorKind::Authentication { message, .. } => format!("Sign-in failed: {message}"),
        ErrorKind::ServerSelection { message, .. } => {
            let reason = message
                .split("Error: ")
                .nth(1)
                .map(|rest| rest.split(", labels").next().unwrap_or(rest))
                .map(|rest| rest.trim_start_matches("Kind: ").trim().to_string());
            match reason {
                Some(reason) if !reason.is_empty() => {
                    format!("Couldn't reach MongoDB: {reason}")
                }
                _ => {
                    "Couldn't reach MongoDB in time. Check the host, port, and TLS setting.".into()
                }
            }
        }
        ErrorKind::InvalidArgument { message, .. } => message.clone(),
        ErrorKind::DnsResolve { message, .. } => format!("DNS lookup failed: {message}"),
        _ => err.to_string(),
    }
}

/// Checks a connection string and returns its first host and port, for display.
pub fn check_uri(uri: &str) -> Result<(String, u16), String> {
    let (srv, rest) = if let Some(rest) = uri.strip_prefix("mongodb+srv://") {
        (true, rest)
    } else if let Some(rest) = uri.strip_prefix("mongodb://") {
        (false, rest)
    } else {
        return Err("A connection string starts with mongodb:// or mongodb+srv://.".into());
    };
    let authority = rest.split(['/', '?']).next().unwrap_or_default();
    if authority.contains('@') {
        return Err(
            "Take the user name and password out of the connection string and enter them in the User and Password fields, so the password stays in the keychain."
                .into(),
        );
    }
    let first = authority.split(',').next().unwrap_or_default();
    if first.is_empty() {
        return Err("The connection string needs a host, like mongodb://db.example.com.".into());
    }
    let (host, port) = if let Some(rest) = first.strip_prefix('[') {
        let (host, after) = rest
            .split_once(']')
            .ok_or("The IPv6 host isn't closed with ].")?;
        (host.to_string(), after.strip_prefix(':'))
    } else {
        match first.rsplit_once(':') {
            Some((host, port)) => (host.to_string(), Some(port)),
            None => (first.to_string(), None),
        }
    };
    let port = match port {
        Some(port) if !srv => port
            .parse::<u16>()
            .map_err(|_| format!("“{port}” isn't a port number."))?,
        Some(_) => return Err("A mongodb+srv:// connection string has no port.".into()),
        None => Driver::Mongo.default_port(),
    };
    Ok((host, port))
}

pub fn check_database_name(name: &str) -> Result<(), String> {
    if name.len() > 63 || name.contains(['/', '\\', '.', ' ', '"', '$', '\0']) {
        return Err(format!(
            "“{name}” can't be a MongoDB database name, which can't contain / \\ . $ quotes or spaces."
        ));
    }
    Ok(())
}

/// TLS files for the driver, which reads them only from disk. They live in a
/// private folder just long enough for the client to load them.
struct PemFiles {
    dir: PathBuf,
    ca: Option<PathBuf>,
    cert_key: Option<PathBuf>,
}

impl Drop for PemFiles {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn write_private(path: &PathBuf, parts: &[&[u8]]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    for part in parts {
        file.write_all(part)?;
        file.write_all(b"\n")?;
    }
    Ok(())
}

fn pem_files(material: &TlsMaterial) -> Result<Option<PemFiles>, String> {
    if material.ca.is_none() && material.cert.is_none() {
        return Ok(None);
    }
    let dir = std::env::temp_dir().join(format!("recon-mongo-{}", uuid::Uuid::new_v4()));
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(&dir)
        .map_err(|err| format!("Could not prepare the TLS certificates: {err}"))?;
    let mut files = PemFiles {
        dir,
        ca: None,
        cert_key: None,
    };
    let failed = |err: std::io::Error| format!("Could not prepare the TLS certificates: {err}");
    if let Some(ca) = &material.ca {
        let path = files.dir.join("ca.pem");
        write_private(&path, &[ca]).map_err(failed)?;
        files.ca = Some(path);
    }
    if let (Some(cert), Some(key)) = (&material.cert, &material.key) {
        let path = files.dir.join("client.pem");
        write_private(&path, &[cert, key]).map_err(failed)?;
        files.cert_key = Some(path);
    }
    Ok(Some(files))
}

async fn client_options(
    target: &ConnectionEntry,
    password: Option<&str>,
) -> Result<ClientOptions, String> {
    let mut options = if target.uri.is_empty() {
        let mut options = ClientOptions::default();
        options.hosts = vec![ServerAddress::Tcp {
            host: target.host.clone(),
            port: Some(target.port),
        }];
        // Through a tunnel only the one server is reachable, so don't look for the others.
        if target.ssh.enabled {
            options.direct_connection = Some(true);
        }
        options
    } else {
        ClientOptions::parse(&target.uri).await.map_err(message)?
    };
    options.app_name = Some("Recon".into());
    options.connect_timeout = Some(CONNECT_TIMEOUT);
    options.server_selection_timeout = Some(CONNECT_TIMEOUT);
    let password = password.filter(|value| !value.is_empty());
    if !target.user.is_empty() || password.is_some() {
        let mut credential = options.credential.take().unwrap_or_default();
        if !target.user.is_empty() {
            credential.username = Some(target.user.clone());
        }
        if let Some(password) = password {
            credential.password = Some(password.to_string());
        }
        if target.uri.is_empty() {
            credential.source = Some(if target.auth_database.is_empty() {
                "admin".into()
            } else {
                target.auth_database.clone()
            });
        }
        options.credential = Some(credential);
    } else if target.uri.is_empty() {
        options.credential = None::<Credential>;
    }
    if target.requires_tls() {
        let mut tls = TlsOptions::default();
        tls.allow_invalid_certificates = Some(!target.verifies_certificate());
        options.tls = Some(Tls::Enabled(tls));
    } else if target.uri.is_empty() {
        options.tls = Some(Tls::Disabled);
    }
    Ok(options)
}

async fn open_client(
    target: &ConnectionEntry,
    password: Option<&str>,
    material: &TlsMaterial,
) -> Result<Client, String> {
    let mut options = client_options(target, password).await?;
    let files = pem_files(material)?;
    if let (Some(files), Some(Tls::Enabled(tls))) = (&files, options.tls.as_mut()) {
        tls.ca_file_path = files.ca.clone();
        tls.cert_key_file_path = files.cert_key.clone();
    }
    // The driver reads the certificate files here, so they can go right after.
    let client = Client::with_options(options).map_err(message);
    drop(files);
    client
}

struct ServerInfo {
    version: String,
    topology: String,
}

impl ServerInfo {
    fn label(&self) -> String {
        format!("MongoDB {}{}", self.version, self.topology)
    }
}

async fn server_info(client: &Client) -> Result<ServerInfo, String> {
    let admin = client.database("admin");
    let hello = match admin.run_command(doc! { "hello": 1 }).await {
        Ok(hello) => hello,
        Err(_) => admin
            .run_command(doc! { "isMaster": 1 })
            .await
            .map_err(message)?,
    };
    let topology = if let Ok(set) = hello.get_str("setName") {
        let role = if hello
            .get_bool("isWritablePrimary")
            .or_else(|_| hello.get_bool("ismaster"))
            .unwrap_or(false)
        {
            "primary"
        } else {
            "secondary"
        };
        format!(" (replica set {set}, {role})")
    } else if hello.get_str("msg") == Ok("isdbgrid") {
        " (sharded)".into()
    } else {
        String::new()
    };
    let version = admin
        .run_command(doc! { "buildInfo": 1 })
        .await
        .ok()
        .and_then(|info| info.get_str("version").ok().map(str::to_string))
        .unwrap_or_else(|| "server".into());
    Ok(ServerInfo { version, topology })
}

pub async fn test(
    target: &ConnectionEntry,
    password: Option<&str>,
    material: &TlsMaterial,
) -> Result<String, String> {
    let client = open_client(target, password, material).await?;
    let info = server_info(&client).await?;
    Ok(format!("Connected to {}", info.label()))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseInfo {
    name: String,
    size_on_disk: u64,
    empty: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MongoInfo {
    server_version: String,
    database: String,
    databases: Vec<DatabaseInfo>,
}

async fn databases(session: &MongoSession, fallback: &str) -> Result<Vec<DatabaseInfo>, String> {
    let listed = session
        .client
        .list_databases()
        .authorized_databases(true)
        .await;
    let mut items: Vec<DatabaseInfo> = match listed {
        Ok(specs) => specs
            .into_iter()
            .map(|spec| DatabaseInfo {
                name: spec.name,
                size_on_disk: spec.size_on_disk,
                empty: spec.empty,
            })
            .collect(),
        Err(err) if fallback.is_empty() => return Err(session.explain(err)),
        Err(_) => Vec::new(),
    };
    if !fallback.is_empty() && !items.iter().any(|item| item.name == fallback) {
        items.push(DatabaseInfo {
            name: fallback.to_string(),
            size_on_disk: 0,
            empty: true,
        });
    }
    items.sort_by(|a, b| {
        let system = |name: &str| SYSTEM_DATABASES.contains(&name);
        system(&a.name)
            .cmp(&system(&b.name))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(items)
}

fn find_entry(state: &AppState, connection_id: &str) -> Result<ConnectionEntry, String> {
    let data = state.data.lock().map_err(|err| err.to_string())?;
    data.find_connection(connection_id)
        .cloned()
        .ok_or_else(|| "Connection not found".to_string())
}

#[tauri::command]
pub async fn mongo_connect(
    app: AppHandle,
    state: State<'_, AppState>,
    store: State<'_, MongoStore>,
    connection_id: String,
    password: Option<String>,
    session_id: Option<String>,
) -> Result<MongoInfo, String> {
    let entry = find_entry(&state, &connection_id)?;
    if entry.driver != Driver::Mongo {
        return Err("This isn't a MongoDB connection.".into());
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
    let opened = async {
        let client = open_client(&target, password.as_deref(), &material).await?;
        let info = server_info(&client).await?;
        let default = client
            .default_database()
            .map(|db| db.name().to_string())
            .unwrap_or_default();
        Ok::<_, String>((client, info, default))
    }
    .await;
    let (client, info, uri_default) = match opened {
        Ok(opened) => opened,
        Err(err) => {
            let err = explain(tunnel.as_deref(), err);
            if let Some(tunnel) = &tunnel {
                ssh::release(tunnel).await;
            }
            return Err(err);
        }
    };
    let preferred = if entry.database.is_empty() {
        uri_default
    } else {
        entry.database.clone()
    };
    let session = Arc::new(MongoSession {
        entry,
        client,
        tunnel,
    });
    let databases = databases(&session, &preferred).await.unwrap_or_default();
    let database = if preferred.is_empty() {
        databases
            .iter()
            .find(|item| !SYSTEM_DATABASES.contains(&item.name.as_str()))
            .or_else(|| databases.first())
            .map(|item| item.name.clone())
            .unwrap_or_else(|| "test".into())
    } else {
        preferred
    };
    if let Some(stale) = store.0.lock().await.insert(key, session) {
        stale.close().await;
    }
    Ok(MongoInfo {
        server_version: info.label(),
        database,
        databases,
    })
}

#[tauri::command]
pub async fn mongo_disconnect(
    store: State<'_, MongoStore>,
    session_id: String,
) -> Result<(), String> {
    if let Some(session) = store.0.lock().await.remove(&session_id) {
        session.close().await;
    }
    Ok(())
}

#[tauri::command]
pub async fn mongo_databases(
    store: State<'_, MongoStore>,
    session_id: String,
    current: String,
) -> Result<Vec<DatabaseInfo>, String> {
    databases(&*store.get(&session_id).await?, &current).await
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionInfo {
    name: String,
    /// collection, view, or timeseries.
    kind: &'static str,
    count: Option<u64>,
}

#[tauri::command]
pub async fn mongo_collections(
    store: State<'_, MongoStore>,
    session_id: String,
    db: String,
) -> Result<Vec<CollectionInfo>, String> {
    let session = store.get(&session_id).await?;
    let database = session.database(&db)?;
    let specs: Vec<_> = database
        .list_collections()
        .await
        .map_err(|err| session.explain(err))?
        .try_collect()
        .await
        .map_err(|err| session.explain(err))?;
    let counts = join_all(specs.iter().map(|spec| {
        let coll = database.collection::<Document>(&spec.name);
        let countable = matches!(spec.collection_type, CollectionType::Collection);
        async move {
            if countable {
                coll.estimated_document_count().await.ok()
            } else {
                None
            }
        }
    }))
    .await;
    let mut items: Vec<CollectionInfo> = specs
        .into_iter()
        .zip(counts)
        .map(|(spec, count)| CollectionInfo {
            name: spec.name,
            kind: match spec.collection_type {
                CollectionType::View => "view",
                CollectionType::Timeseries => "timeseries",
                _ => "collection",
            },
            count,
        })
        .collect();
    items.sort_by(|a, b| {
        a.name
            .starts_with("system.")
            .cmp(&b.name.starts_with("system."))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(items)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldCell {
    name: String,
    /// What the grid shows: null, a boolean, a number, or text.
    value: serde_json::Value,
    kind: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentRow {
    /// The `_id`, as relaxed Extended JSON, to find the document again.
    id: Option<serde_json::Value>,
    /// The whole document as mongosh-style text, for viewing and editing.
    text: String,
    fields: Vec<FieldCell>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FindPage {
    documents: Vec<DocumentRow>,
    /// Documents matching the filter, when counting them didn't take too long.
    total: Option<u64>,
    duration_ms: u64,
}

fn cell(value: &Bson) -> (serde_json::Value, &'static str) {
    const SAFE: i64 = 1 << 53;
    use serde_json::json;
    match value {
        Bson::String(text) => (json!(text), "string"),
        Bson::Int32(n) => (json!(n), "int"),
        Bson::Int64(n) if (-SAFE..=SAFE).contains(n) => (json!(n), "long"),
        Bson::Int64(n) => (json!(n.to_string()), "long"),
        Bson::Double(n) if n.is_finite() => (json!(n), "double"),
        Bson::Double(_) => (json!(literal::scalar_text(value)), "double"),
        Bson::Boolean(b) => (json!(b), "bool"),
        Bson::Null => (serde_json::Value::Null, "null"),
        Bson::Decimal128(d) => (json!(d.to_string()), "decimal"),
        Bson::ObjectId(id) => (json!(id.to_hex()), "objectId"),
        Bson::DateTime(_) => (json!(summary(value)), "date"),
        Bson::Document(_) => (json!(summary(value)), "object"),
        Bson::Array(_) => (json!(summary(value)), "array"),
        Bson::Binary(_) => (json!(summary(value)), "binary"),
        Bson::RegularExpression(_) => (json!(summary(value)), "regex"),
        Bson::Timestamp(_) => (json!(summary(value)), "timestamp"),
        _ => (json!(summary(value)), "other"),
    }
}

fn row(doc: &Document) -> DocumentRow {
    DocumentRow {
        id: doc.get("_id").map(to_json),
        text: document_text(doc),
        fields: doc
            .iter()
            .map(|(name, value)| {
                let (value, kind) = cell(value);
                FieldCell {
                    name: name.clone(),
                    value,
                    kind,
                }
            })
            .collect(),
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn parse_part(text: &str, what: &str) -> Result<Document, String> {
    parse_document(text).map_err(|err| format!("The {what} has a mistake: {err}"))
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn mongo_find(
    store: State<'_, MongoStore>,
    session_id: String,
    db: String,
    collection: String,
    filter: String,
    sort: String,
    projection: String,
    skip: u64,
    limit: i64,
) -> Result<FindPage, String> {
    let session = store.get(&session_id).await?;
    let coll = session.collection(&db, &collection)?;
    let filter = parse_part(&filter, "filter")?;
    let sort = parse_part(&sort, "sort")?;
    let projection = parse_part(&projection, "projection")?;
    let started = Instant::now();
    let mut find = coll
        .find(filter.clone())
        .skip(skip)
        .limit(limit.clamp(1, PAGE_MAX))
        .max_time(QUERY_TIMEOUT);
    if !sort.is_empty() {
        find = find.sort(sort);
    }
    if !projection.is_empty() {
        find = find.projection(projection);
    }
    let docs: Vec<Document> = find
        .await
        .map_err(|err| session.explain(err))?
        .try_collect()
        .await
        .map_err(|err| session.explain(err))?;
    let duration = started.elapsed();
    let total = if filter.is_empty() {
        coll.estimated_document_count().await.ok()
    } else {
        coll.count_documents(filter)
            .max_time(COUNT_TIMEOUT)
            .await
            .ok()
    };
    Ok(FindPage {
        documents: docs.iter().map(row).collect(),
        total,
        duration_ms: millis(duration),
    })
}

fn id_filter(id: serde_json::Value) -> Result<Document, String> {
    Ok(doc! { "_id": literal::from_json(id)? })
}

fn shell_call(collection: &str, method: &str, args: &[&Bson]) -> String {
    let args: Vec<String> = args
        .iter()
        .map(|arg| {
            to_text(arg)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    format!(
        "db.getCollection({}).{method}({})",
        literal::scalar_text(&Bson::String(collection.into())),
        args.join(", ")
    )
}

#[tauri::command]
pub async fn mongo_insert(
    store: State<'_, MongoStore>,
    session_id: String,
    db: String,
    collection: String,
    document: String,
) -> Result<DocumentRow, String> {
    let session = store.get(&session_id).await?;
    let coll = session.collection(&db, &collection)?;
    let mut doc = parse_part(&document, "document")?;
    let text = shell_call(&collection, "insertOne", &[&Bson::Document(doc.clone())]);
    let started = Instant::now();
    let outcome = coll
        .insert_one(doc.clone())
        .await
        .map_err(|err| session.explain(err));
    session.record(&db, &text, QueryOrigin::Edit, started, &outcome);
    let inserted = outcome?;
    if !doc.contains_key("_id") {
        let mut with_id = doc! { "_id": inserted.inserted_id };
        with_id.extend(doc);
        doc = with_id;
    }
    Ok(row(&doc))
}

#[tauri::command]
pub async fn mongo_replace(
    store: State<'_, MongoStore>,
    session_id: String,
    db: String,
    collection: String,
    id: serde_json::Value,
    document: String,
) -> Result<DocumentRow, String> {
    let session = store.get(&session_id).await?;
    let coll = session.collection(&db, &collection)?;
    let filter = id_filter(id)?;
    let doc = parse_part(&document, "document")?;
    if let Some(new_id) = doc.get("_id") {
        if Some(new_id) != filter.get("_id") {
            return Err(
                "A document's _id can't change. To keep it under a new _id, insert a copy and delete this one."
                    .into(),
            );
        }
    }
    let text = shell_call(
        &collection,
        "replaceOne",
        &[
            &Bson::Document(filter.clone()),
            &Bson::Document(doc.clone()),
        ],
    );
    let started = Instant::now();
    let outcome = coll
        .replace_one(filter.clone(), doc)
        .await
        .map_err(|err| session.explain(err))
        .and_then(|result| {
            if result.matched_count == 0 {
                Err("This document is gone. Someone may have deleted it. Refresh to see the collection now.".to_string())
            } else {
                Ok(result)
            }
        });
    session.record(&db, &text, QueryOrigin::Edit, started, &outcome);
    outcome?;
    let saved = coll
        .find_one(filter)
        .await
        .map_err(|err| session.explain(err))?
        .ok_or("The document was saved, but couldn't be read back.")?;
    Ok(row(&saved))
}

/// The edited text as the same type the field had, so an edit never changes a type by accident.
fn coerce(old: Option<&Bson>, text: &str) -> Result<Bson, String> {
    let trimmed = text.trim();
    let keep = |kind: &str| {
        format!("“{trimmed}” isn't {kind}. To change the field's type, edit the document as text.")
    };
    Ok(match old {
        Some(Bson::String(_)) => Bson::String(text.to_string()),
        Some(Bson::Int32(_)) => Bson::Int32(trimmed.parse().map_err(|_| keep("a 32-bit integer"))?),
        Some(Bson::Int64(_)) => Bson::Int64(trimmed.parse().map_err(|_| keep("a 64-bit integer"))?),
        Some(Bson::Double(_)) => Bson::Double(trimmed.parse().map_err(|_| keep("a number"))?),
        Some(Bson::Decimal128(_)) => {
            Bson::Decimal128(trimmed.parse().map_err(|_| keep("a decimal"))?)
        }
        Some(Bson::Boolean(_)) => match trimmed.to_ascii_lowercase().as_str() {
            "true" | "1" => Bson::Boolean(true),
            "false" | "0" => Bson::Boolean(false),
            _ => return Err(keep("true or false")),
        },
        Some(Bson::DateTime(_)) => Bson::DateTime(literal::parse_date(trimmed)?),
        Some(Bson::ObjectId(_)) => Bson::ObjectId(
            mongodb::bson::oid::ObjectId::parse_str(trimmed).map_err(|_| keep("an ObjectId"))?,
        ),
        Some(Bson::Null) | None => {
            literal::parse_value(trimmed).unwrap_or_else(|_| Bson::String(text.to_string()))
        }
        Some(other) => {
            return Err(format!(
                "{} can't be edited in the grid. Edit the document as text instead.",
                literal::type_name(other)
            ))
        }
    })
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn mongo_set_field(
    store: State<'_, MongoStore>,
    session_id: String,
    db: String,
    collection: String,
    id: serde_json::Value,
    field: String,
    // The new value as text, or null to set the field to null.
    text: Option<String>,
) -> Result<DocumentRow, String> {
    let session = store.get(&session_id).await?;
    let coll = session.collection(&db, &collection)?;
    if field == "_id" {
        return Err("A document's _id can't change.".into());
    }
    if field.starts_with('$') || field.contains('.') {
        return Err("Fields with $ or . in their names can only be edited as text.".into());
    }
    let filter = id_filter(id)?;
    let current = coll
        .find_one(filter.clone())
        .await
        .map_err(|err| session.explain(err))?
        .ok_or("This document is gone. Someone may have deleted it. Refresh to see the collection now.")?;
    let value = match text {
        None => Bson::Null,
        Some(text) => coerce(current.get(&field), &text)?,
    };
    let mut set = Document::new();
    set.insert(field, value);
    let update = doc! { "$set": set };
    let text = shell_call(
        &collection,
        "updateOne",
        &[
            &Bson::Document(filter.clone()),
            &Bson::Document(update.clone()),
        ],
    );
    let started = Instant::now();
    let outcome = coll
        .update_one(filter.clone(), update)
        .await
        .map_err(|err| session.explain(err));
    session.record(&db, &text, QueryOrigin::Edit, started, &outcome);
    outcome?;
    let saved = coll
        .find_one(filter)
        .await
        .map_err(|err| session.explain(err))?
        .ok_or("The change was saved, but the document couldn't be read back.")?;
    Ok(row(&saved))
}

#[tauri::command]
pub async fn mongo_delete(
    store: State<'_, MongoStore>,
    session_id: String,
    db: String,
    collection: String,
    ids: Vec<serde_json::Value>,
) -> Result<u64, String> {
    let session = store.get(&session_id).await?;
    let coll = session.collection(&db, &collection)?;
    if ids.is_empty() {
        return Ok(0);
    }
    let ids = ids
        .into_iter()
        .map(literal::from_json)
        .collect::<Result<Vec<_>, _>>()?;
    let filter = doc! { "_id": { "$in": ids } };
    let text = shell_call(
        &collection,
        "deleteMany",
        &[&Bson::Document(filter.clone())],
    );
    let started = Instant::now();
    let outcome = coll
        .delete_many(filter)
        .await
        .map(|result| result.deleted_count)
        .map_err(|err| session.explain(err));
    session.record(&db, &text, QueryOrigin::Edit, started, &outcome);
    outcome
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexInfo {
    name: String,
    /// The keys, such as `{ "email": 1 }`, on one line.
    keys: String,
    unique: bool,
    sparse: bool,
    hidden: bool,
    ttl_seconds: Option<i64>,
    partial: Option<String>,
    /// The whole index definition.
    text: String,
}

fn one_line(value: &Bson) -> String {
    to_text(value)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[tauri::command]
pub async fn mongo_indexes(
    store: State<'_, MongoStore>,
    session_id: String,
    db: String,
    collection: String,
) -> Result<Vec<IndexInfo>, String> {
    let session = store.get(&session_id).await?;
    let database = session.database(&db)?;
    let reply = database
        .run_command(doc! { "listIndexes": &collection })
        .await
        .map_err(|err| session.explain(err))?;
    let batch = reply
        .get_document("cursor")
        .and_then(|cursor| cursor.get_array("firstBatch"))
        .cloned()
        .unwrap_or_default();
    Ok(batch
        .iter()
        .filter_map(|spec| spec.as_document())
        .map(|spec| IndexInfo {
            name: spec.get_str("name").unwrap_or_default().to_string(),
            keys: spec.get("key").map(one_line).unwrap_or_default(),
            unique: spec.get_bool("unique").unwrap_or(false),
            sparse: spec.get_bool("sparse").unwrap_or(false),
            hidden: spec.get_bool("hidden").unwrap_or(false),
            ttl_seconds: spec.get("expireAfterSeconds").and_then(|value| {
                value
                    .as_i64()
                    .or_else(|| value.as_i32().map(i64::from))
                    .or_else(|| value.as_f64().map(|f| f as i64))
            }),
            partial: spec.get("partialFilterExpression").map(one_line),
            text: document_text(spec),
        })
        .collect())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleResult {
    output: String,
    error: bool,
    duration_ms: u64,
    /// The command may have changed data or collections.
    changed: bool,
    /// `use name` switched to this database.
    database: Option<String>,
}

fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

async fn collect_limited(
    cursor: mongodb::Cursor<Document>,
    limit: i64,
) -> Result<(Vec<Bson>, bool), mongodb::error::Error> {
    let mut cursor = cursor;
    let mut docs = Vec::new();
    while let Some(doc) = cursor.try_next().await? {
        if docs.len() as i64 >= limit {
            return Ok((docs, true));
        }
        docs.push(Bson::Document(doc));
    }
    Ok((docs, false))
}

fn documents_output(docs: Vec<Bson>, more: bool, limit: i64, hint: &str) -> String {
    if docs.is_empty() {
        return "No documents.".into();
    }
    let count = docs.len();
    let mut out = to_text(&Bson::Array(docs));
    if more {
        out.push_str(&format!("\n\nShowing the first {limit}. {hint}"));
    } else {
        out.push_str(&format!(
            "\n\n{count} {}",
            if count == 1 { "document" } else { "documents" }
        ));
    }
    out
}

async fn execute(
    session: &MongoSession,
    db: &str,
    command: Command,
) -> Result<(String, Option<String>), mongodb::error::Error> {
    let client = &session.client;
    let database = client.database(db);
    let output = match command {
        Command::ShowDatabases => {
            let specs = client.list_databases().authorized_databases(true).await?;
            let width = specs.iter().map(|spec| spec.name.len()).max().unwrap_or(0);
            specs
                .iter()
                .map(|spec| {
                    format!(
                        "{:width$}  {}",
                        spec.name,
                        format_size(spec.size_on_disk),
                        width = width
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        Command::ShowCollections | Command::CollectionNames => {
            let mut names = database.list_collection_names().await?;
            names.sort();
            if names.is_empty() {
                "No collections.".into()
            } else {
                names.join("\n")
            }
        }
        Command::Use(name) => {
            return Ok((format!("Switched to database {name}."), Some(name)));
        }
        Command::Run { admin, command } => {
            let target = if admin {
                client.database("admin")
            } else {
                database
            };
            document_text(&target.run_command(command).await?)
        }
        Command::CreateCollection { name, options } => {
            let mut command = doc! { "create": name };
            command.extend(options);
            document_text(&database.run_command(command).await?)
        }
        Command::DropDatabase => {
            database.drop().await?;
            format!("Dropped the database {db}.")
        }
        Command::Find(find) => {
            let coll = database.collection::<Document>(&find.coll);
            if find.one {
                let mut action = coll.find_one(find.filter);
                if let Some(projection) = find.projection {
                    action = action.projection(projection);
                }
                match action.await? {
                    Some(doc) => document_text(&doc),
                    None => "null".into(),
                }
            } else if find.count {
                coll.count_documents(find.filter).await?.to_string()
            } else {
                let limit = find
                    .limit
                    .filter(|n| *n > 0)
                    .unwrap_or(CONSOLE_LIMIT)
                    .min(CONSOLE_MAX);
                let mut action = coll
                    .find(find.filter)
                    .limit(limit + 1)
                    .max_time(QUERY_TIMEOUT);
                if let Some(projection) = find.projection {
                    action = action.projection(projection);
                }
                if let Some(sort) = find.sort {
                    action = action.sort(sort);
                }
                if let Some(skip) = find.skip {
                    action = action.skip(skip);
                }
                let (docs, more) = collect_limited(action.await?, limit).await?;
                documents_output(docs, more, limit, "Add .limit(n), up to 1000, to see more.")
            }
        }
        Command::Count { coll, filter } => database
            .collection::<Document>(&coll)
            .count_documents(filter)
            .await?
            .to_string(),
        Command::EstimatedCount { coll } => database
            .collection::<Document>(&coll)
            .estimated_document_count()
            .await?
            .to_string(),
        Command::Distinct { coll, key, filter } => {
            let values = database
                .collection::<Document>(&coll)
                .distinct(key, filter)
                .await?;
            to_text(&Bson::Array(values))
        }
        Command::Aggregate { coll, pipeline } => {
            let cursor = database
                .collection::<Document>(&coll)
                .aggregate(pipeline)
                .max_time(QUERY_TIMEOUT)
                .await?;
            let (docs, more) = collect_limited(cursor, CONSOLE_MAX).await?;
            documents_output(docs, more, CONSOLE_MAX, "Add a $limit stage to see fewer.")
        }
        Command::InsertOne { coll, doc } => {
            let result = database
                .collection::<Document>(&coll)
                .insert_one(doc)
                .await?;
            document_text(&doc! { "acknowledged": true, "insertedId": result.inserted_id })
        }
        Command::InsertMany { coll, docs } => {
            let result = database
                .collection::<Document>(&coll)
                .insert_many(docs)
                .await?;
            let mut ids: Vec<(usize, Bson)> = result.inserted_ids.into_iter().collect();
            ids.sort_by_key(|(index, _)| *index);
            let ids: Vec<Bson> = ids.into_iter().map(|(_, id)| id).collect();
            document_text(&doc! { "acknowledged": true, "insertedIds": ids })
        }
        Command::Update {
            coll,
            filter,
            update,
            many,
            upsert,
        } => {
            let coll = database.collection::<Document>(&coll);
            let result = match (update, many) {
                (Bson::Array(stages), many) => {
                    let pipeline: Vec<Document> = stages
                        .into_iter()
                        .filter_map(|stage| stage.as_document().cloned())
                        .collect();
                    if many {
                        coll.update_many(filter, pipeline).upsert(upsert).await?
                    } else {
                        coll.update_one(filter, pipeline).upsert(upsert).await?
                    }
                }
                (update, true) => {
                    let update = update.as_document().cloned().unwrap_or_default();
                    coll.update_many(filter, update).upsert(upsert).await?
                }
                (update, false) => {
                    let update = update.as_document().cloned().unwrap_or_default();
                    coll.update_one(filter, update).upsert(upsert).await?
                }
            };
            let mut out = doc! {
                "acknowledged": true,
                "matchedCount": result.matched_count as i64,
                "modifiedCount": result.modified_count as i64,
            };
            if let Some(id) = result.upserted_id {
                out.insert("upsertedId", id);
            }
            document_text(&out)
        }
        Command::Replace {
            coll,
            filter,
            doc,
            upsert,
        } => {
            let result = database
                .collection::<Document>(&coll)
                .replace_one(filter, doc)
                .upsert(upsert)
                .await?;
            let mut out = doc! {
                "acknowledged": true,
                "matchedCount": result.matched_count as i64,
                "modifiedCount": result.modified_count as i64,
            };
            if let Some(id) = result.upserted_id {
                out.insert("upsertedId", id);
            }
            document_text(&out)
        }
        Command::Delete { coll, filter, many } => {
            let coll = database.collection::<Document>(&coll);
            let result = if many {
                coll.delete_many(filter).await?
            } else {
                coll.delete_one(filter).await?
            };
            document_text(
                &doc! { "acknowledged": true, "deletedCount": result.deleted_count as i64 },
            )
        }
        Command::CreateIndex {
            coll,
            keys,
            mut options,
        } => {
            if !options.contains_key("name") {
                let name: Vec<String> = keys
                    .iter()
                    .map(|(key, value)| format!("{key}_{}", literal::summary(value)))
                    .collect();
                options.insert("name", name.join("_"));
            }
            let mut index = doc! { "key": keys };
            index.extend(options);
            let reply = database
                .run_command(doc! { "createIndexes": coll, "indexes": [index.clone()] })
                .await?;
            match reply
                .get_i32("numIndexesAfter")
                .ok()
                .zip(reply.get_i32("numIndexesBefore").ok())
            {
                Some((after, before)) if after == before => {
                    format!(
                        "The index {} already exists.",
                        index.get_str("name").unwrap_or_default()
                    )
                }
                _ => index.get_str("name").unwrap_or_default().to_string(),
            }
        }
        Command::DropIndex { coll, index } => document_text(
            &database
                .run_command(doc! { "dropIndexes": coll, "index": index })
                .await?,
        ),
        Command::Indexes { coll } => {
            let reply = database.run_command(doc! { "listIndexes": coll }).await?;
            let batch = reply
                .get_document("cursor")
                .and_then(|cursor| cursor.get_array("firstBatch"))
                .cloned()
                .unwrap_or_default();
            to_text(&Bson::Array(batch))
        }
        Command::Drop { coll } => {
            database.collection::<Document>(&coll).drop().await?;
            format!("Dropped the collection {coll}.")
        }
    };
    Ok((output, None))
}

#[tauri::command]
pub async fn mongo_command(
    store: State<'_, MongoStore>,
    session_id: String,
    db: String,
    line: String,
) -> Result<ConsoleResult, String> {
    let session = store.get(&session_id).await?;
    let command = match console::parse(&line) {
        Ok(command) => command,
        Err(err) => {
            return Ok(ConsoleResult {
                output: err,
                error: true,
                duration_ms: 0,
                changed: false,
                database: None,
            })
        }
    };
    if let Command::Use(name) = &command {
        check_database_name(name)?;
    } else if db.is_empty() {
        return Err("Choose a database first.".into());
    }
    let changed = command.changes();
    let started = Instant::now();
    let outcome = execute(&session, &db, command)
        .await
        .map_err(|err| session.explain(err));
    let duration = started.elapsed();
    session.record(&db, line.trim(), QueryOrigin::Editor, started, &outcome);
    Ok(match outcome {
        Ok((output, database)) => ConsoleResult {
            output,
            error: false,
            duration_ms: millis(duration),
            changed,
            database,
        },
        Err(err) => ConsoleResult {
            output: err,
            error: true,
            duration_ms: millis(duration),
            changed: false,
            database: None,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_connection_strings() {
        assert_eq!(
            check_uri("mongodb://db1.example.com:27018,db2.example.com/app?replicaSet=rs0")
                .unwrap(),
            ("db1.example.com".into(), 27018)
        );
        assert_eq!(
            check_uri("mongodb+srv://cluster0.abcde.mongodb.net/?retryWrites=true").unwrap(),
            ("cluster0.abcde.mongodb.net".into(), 27017)
        );
        assert_eq!(
            check_uri("mongodb://[::1]:27019").unwrap(),
            ("::1".into(), 27019)
        );
        assert!(check_uri("mongodb://ada:secret@db.example.com").is_err());
        assert!(check_uri("mongodb://ada@db.example.com").is_err());
        assert!(check_uri("postgres://db").is_err());
        assert!(check_uri("mongodb+srv://cluster0.example.com:27017").is_err());
    }

    #[test]
    fn checks_database_names() {
        assert!(check_database_name("").is_ok());
        assert!(check_database_name("shop_2024").is_ok());
        assert!(check_database_name("my.db").is_err());
        assert!(check_database_name("a b").is_err());
    }

    #[test]
    fn keeps_field_types_when_coercing() {
        assert_eq!(
            coerce(Some(&Bson::Int64(1)), " 42 ").unwrap(),
            Bson::Int64(42)
        );
        assert_eq!(coerce(Some(&Bson::Int32(1)), "7").unwrap(), Bson::Int32(7));
        assert_eq!(
            coerce(Some(&Bson::Double(1.0)), "2").unwrap(),
            Bson::Double(2.0)
        );
        assert!(coerce(Some(&Bson::Int32(1)), "abc").is_err());
        assert_eq!(
            coerce(Some(&Bson::String("a".into())), " 5 ").unwrap(),
            Bson::String(" 5 ".into())
        );
        assert_eq!(
            coerce(Some(&Bson::Boolean(true)), "false").unwrap(),
            Bson::Boolean(false)
        );
        assert_eq!(coerce(None, "12").unwrap(), Bson::Int32(12));
        assert_eq!(
            coerce(Some(&Bson::Null), "hello").unwrap(),
            Bson::String("hello".into())
        );
        assert!(coerce(Some(&Bson::Array(vec![])), "x").is_err());
    }

    #[test]
    fn writes_shell_calls_for_history() {
        assert_eq!(
            shell_call("users", "deleteMany", &[&Bson::Document(doc! { "a": 1 })]),
            "db.getCollection(\"users\").deleteMany({ \"a\": 1 })"
        );
    }
}
