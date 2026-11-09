use std::sync::atomic::{AtomicBool, Ordering};

use deadpool::managed::{Manager, Metrics, Object, Pool, RecycleError, RecycleResult};
use futures_util::TryStreamExt;
use tiberius::{AuthMethod, Client, ColumnData, ColumnType, Config, EncryptionLevel, QueryItem};
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

use super::{
    quote_bracket, text_at, texts, CellValue, ColumnChange, ColumnMeta, Conn, Dialect,
    EditStatement, Opened, Pool as SessionPool, RawOutput, RowValues, CONNECTION_LOST,
    CONNECT_TIMEOUT,
};
use crate::models::ConnectionEntry;

const MAX_POOL: usize = 4;

pub type MssqlClient = Client<Compat<TcpStream>>;

#[derive(Clone)]
pub struct MssqlPool {
    pool: Pool<MssqlManager>,
}

pub struct MssqlManager {
    config: Config,
}

impl Manager for MssqlManager {
    type Type = MssqlClient;
    type Error = String;

    async fn create(&self) -> Result<MssqlClient, String> {
        connect_config(&self.config).await
    }

    /// A cancelled browse ends its session with `KILL`, so idle connections are checked before reuse.
    async fn recycle(&self, client: &mut MssqlClient, _metrics: &Metrics) -> RecycleResult<String> {
        client
            .simple_query("SELECT 1")
            .await
            .map_err(|err| RecycleError::Message(describe(err).into()))?
            .into_results()
            .await
            .map_err(|err| RecycleError::Message(describe(err).into()))?;
        Ok(())
    }
}

pub struct MssqlConn(MssqlClient);

impl MssqlConn {
    pub fn client(&mut self) -> &mut MssqlClient {
        &mut self.0
    }

    pub async fn close(self) {
        let _ = self.0.close().await;
    }
}

/// A column's live definition, read back before an ALTER so defaults survive.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnDefinition {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub default_value: Option<String>,
    pub default_constraint: Option<String>,
    pub computed: bool,
    pub identity: bool,
}

impl MssqlPool {
    pub fn status(&self) -> deadpool::Status {
        self.pool.status()
    }

    pub async fn close(&self) {
        self.pool.close();
    }

    pub async fn checkout(&self) -> Result<Object<MssqlManager>, String> {
        match tokio::time::timeout(CONNECT_TIMEOUT, self.pool.get()).await {
            Ok(Ok(object)) => Ok(object),
            Ok(Err(err)) => Err(err.to_string()),
            Err(_) => Err("Timed out waiting for a database connection.".into()),
        }
    }

    /// Drops idle connections so a database rename isn't blocked by this app.
    pub async fn quiesce(&self) {
        for client in self.pool.retain(|_, _| false).removed {
            let _ = client.close().await;
        }
    }

    pub async fn run(&self, sql: &str, limit: usize) -> Result<RawOutput, String> {
        let mut object = self.checkout().await?;
        let result = run(object.as_mut(), sql, limit, None).await;
        if result.is_err() {
            let _ = Object::take(object).close().await;
        }
        result
    }

    pub async fn apply(&self, statements: &[EditStatement]) -> Result<(), String> {
        let mut object = self.checkout().await?;
        let result = apply(object.as_mut(), statements).await;
        if result.is_err() {
            let _ = Object::take(object).close().await;
        }
        result
    }

    pub async fn detached(&self) -> Result<MssqlConn, String> {
        self.checkout()
            .await
            .map(|object| MssqlConn(Object::take(object)))
    }
}

fn config_for(entry: &ConnectionEntry, password: Option<&str>) -> Config {
    let mut config = Config::new();
    config.host(&entry.host);
    config.port(entry.port);
    config.authentication(AuthMethod::sql_server(&entry.user, password.unwrap_or("")));
    config.application_name("Recon");
    if !entry.database.is_empty() {
        config.database(&entry.database);
    }
    /*
     * Like Postgres, only the verify modes check the certificate. Most SQL
     * Server installs use the self-signed one it generates at startup.
     * Login still handshakes with encryption off, so that trusts it too.
     */
    match entry.ssl_mode.as_str() {
        "disable" => {
            config.encryption(EncryptionLevel::NotSupported);
            config.trust_cert();
        }
        "verify-ca" | "verify-full" => config.encryption(EncryptionLevel::Required),
        "prefer" => {
            config.encryption(EncryptionLevel::On);
            config.trust_cert();
        }
        _ => {
            config.encryption(EncryptionLevel::Required);
            config.trust_cert();
        }
    }
    config
}

async fn connect_config(config: &Config) -> Result<MssqlClient, String> {
    // The rustls handshake uses more stack than a Tokio worker has left once
    // this runs underneath the connect command. Test connection is shallow
    // enough to succeed; opening the pool overflowed and aborted the process.
    let config = config.clone();
    let handle = tokio::runtime::Handle::current();
    let (send, recv) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("recon-mssql".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let result = handle.block_on(handshake(config));
            let _ = send.send(result);
        })
        .map_err(|err| format!("Could not start the SQL Server connection: {err}"))?;
    recv.await
        .map_err(|_| "The SQL Server connection stopped before it finished.".to_string())?
}

async fn handshake(config: Config) -> Result<MssqlClient, String> {
    let tcp =
        match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(config.get_addr())).await {
            Ok(Ok(tcp)) => tcp,
            Ok(Err(err)) => return Err(format!("Could not reach the database server: {err}")),
            Err(_) => return Err("Timed out connecting to the database server.".into()),
        };
    tcp.set_nodelay(true).map_err(|err| err.to_string())?;
    match tokio::time::timeout(CONNECT_TIMEOUT, Client::connect(config, tcp.compat_write())).await {
        Ok(Ok(client)) => Ok(client),
        Ok(Err(err)) => Err(describe(err)),
        Err(_) => Err("Timed out connecting to the database server.".into()),
    }
}

pub async fn open(entry: &ConnectionEntry, password: Option<&str>) -> Result<Opened, String> {
    let manager = MssqlManager {
        config: config_for(entry, password),
    };
    let pool = Pool::builder(manager)
        .max_size(MAX_POOL)
        .build()
        .map_err(|err| err.to_string())?;
    let pool = MssqlPool { pool };
    let mut conn = match pool.detached().await {
        Ok(conn) => conn,
        Err(err) => {
            pool.close().await;
            return Err(err);
        }
    };
    if let Err(err) = ensure_supported(conn.client()).await {
        conn.close().await;
        pool.close().await;
        return Err(err);
    }
    let backend_id = run(conn.client(), "SELECT @@SPID", 1, None)
        .await
        .ok()
        .and_then(|output| output.rows.first()?.first()?.as_i64());
    Ok(Opened {
        pool: SessionPool::Mssql(pool),
        conn: Conn::Mssql(Box::new(conn)),
        backend_id,
    })
}

pub async fn test(entry: &ConnectionEntry, password: Option<&str>) -> Result<String, String> {
    let mut client = connect_config(&config_for(entry, password)).await?;
    let version = ensure_supported(&mut client).await;
    let _ = client.close().await;
    version
}

/// SQL Server 2022 is major version 16. Developer and Enterprise speak the same dialect.
async fn ensure_supported(client: &mut MssqlClient) -> Result<String, String> {
    let output = run(
        client,
        "SELECT CONVERT(nvarchar(32), SERVERPROPERTY('ProductMajorVersion')), \
         CONVERT(nvarchar(128), SERVERPROPERTY('ProductVersion'))",
        1,
        None,
    )
    .await?;
    let row = output.rows.first();
    let version = row
        .and_then(|row| row.get(1))
        .and_then(CellValue::to_text)
        .unwrap_or_default();
    let major = row
        .and_then(|row| row.first())
        .and_then(CellValue::to_text)
        .unwrap_or_default()
        .trim()
        .parse::<i32>()
        .unwrap_or(0);
    version_gate(major, &version)
}

pub(crate) fn version_gate(major: i32, version: &str) -> Result<String, String> {
    if major < 16 {
        let shown = if version.is_empty() {
            "an older release".into()
        } else {
            version.to_string()
        };
        return Err(format!(
            "Recon supports SQL Server 2022 or newer. This server is {shown}."
        ));
    }
    Ok(version.to_string())
}

pub fn identity_insert_sql(table: &str, enabled: bool) -> String {
    format!(
        "SET IDENTITY_INSERT {table} {}",
        if enabled { "ON" } else { "OFF" }
    )
}

pub async fn run(
    client: &mut MssqlClient,
    sql: &str,
    limit: usize,
    cancel: Option<&AtomicBool>,
) -> Result<RawOutput, String> {
    // A view, procedure, function, or trigger has to be alone in its batch.
    let count_row = !super::returns_rows(sql) && defined_module(sql).is_none();
    let query = if count_row {
        format!("{sql}\nSELECT @@ROWCOUNT")
    } else {
        sql.to_string()
    };
    let mut stream = client.simple_query(query).await.map_err(describe)?;
    let mut sets: Vec<(Vec<ColumnMeta>, Vec<RowValues>)> = Vec::new();
    let mut columns: Option<Vec<ColumnMeta>> = None;
    let mut rows = Vec::new();
    let mut truncated = false;
    while let Some(item) = stream.try_next().await.map_err(describe)? {
        if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            drop(stream);
            let _ = client.cancel_query().await;
            return Err("Query cancelled.".into());
        }
        match item {
            QueryItem::Metadata(meta) => {
                if let Some(columns) = columns.take() {
                    sets.push((columns, std::mem::take(&mut rows)));
                }
                columns = Some(
                    meta.columns()
                        .iter()
                        .map(|column| ColumnMeta {
                            name: column.name().to_string(),
                            type_name: type_name(column.column_type()).to_string(),
                        })
                        .collect(),
                );
            }
            QueryItem::Row(row) => {
                let first = sets.is_empty();
                if !count_row && first && rows.len() >= limit {
                    truncated = true;
                    break;
                }
                if count_row && first && rows.len() >= limit {
                    continue;
                }
                rows.push(read_row(&row));
            }
        }
    }
    if truncated {
        drop(stream);
        let _ = client.cancel_query().await;
    }
    if let Some(columns) = columns {
        sets.push((columns, rows));
    }
    let rows_affected = if count_row {
        sets.pop()
            .and_then(|(_, rows)| rows.first()?.first()?.as_i64())
            .and_then(|count| u64::try_from(count).ok())
            .unwrap_or(0)
    } else {
        0
    };
    let (columns, rows) = sets
        .into_iter()
        .next()
        .unwrap_or_else(|| (Vec::new(), Vec::new()));
    Ok(RawOutput {
        columns,
        rows,
        rows_affected,
        truncated,
    })
}

/// What a `CREATE`, `ALTER`, or `CREATE OR ALTER` batch defines, when it's a view or routine.
pub(crate) fn defined_module(sql: &str) -> Option<&'static str> {
    let words = leading_words(sql, 4);
    let kind = match words.first()?.as_str() {
        "CREATE" if words.get(1).is_some_and(|word| word == "OR") => words.get(3)?,
        "CREATE" | "ALTER" => words.get(1)?,
        _ => return None,
    };
    match kind.as_str() {
        "VIEW" => Some("VIEW"),
        "PROC" | "PROCEDURE" => Some("PROCEDURE"),
        "FUNCTION" => Some("FUNCTION"),
        "TRIGGER" => Some("TRIGGER"),
        _ => None,
    }
}

/// The first `count` words of `sql`, uppercased, skipping comments.
fn leading_words(sql: &str, count: usize) -> Vec<String> {
    let mut words = Vec::new();
    let mut rest = sql;
    while words.len() < count {
        rest = rest.trim_start();
        if let Some(after) = rest.strip_prefix("--") {
            rest = after.split_once('\n').map_or("", |(_, tail)| tail);
        } else if let Some(after) = rest.strip_prefix("/*") {
            rest = after.split_once("*/").map_or("", |(_, tail)| tail);
        } else {
            let end = rest
                .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            if end == 0 {
                break;
            }
            words.push(rest[..end].to_ascii_uppercase());
            rest = &rest[end..];
        }
    }
    words
}

fn read_row(row: &tiberius::Row) -> RowValues {
    (0..row.len()).map(|index| cell(row, index)).collect()
}

pub fn cell(row: &tiberius::Row, index: usize) -> CellValue {
    let kind = row.columns().get(index).map(|column| column.column_type());
    let data = match row.get_column_data(index) {
        Ok(data) => data,
        Err(_) => return CellValue::Text(String::new()),
    };
    match data {
        ColumnData::U8(None)
        | ColumnData::I16(None)
        | ColumnData::I32(None)
        | ColumnData::I64(None)
        | ColumnData::F32(None)
        | ColumnData::F64(None)
        | ColumnData::Bit(None)
        | ColumnData::String(None)
        | ColumnData::Guid(None)
        | ColumnData::Binary(None)
        | ColumnData::Numeric(None)
        | ColumnData::Xml(None)
        | ColumnData::DateTime(None)
        | ColumnData::SmallDateTime(None)
        | ColumnData::Time(None)
        | ColumnData::Date(None)
        | ColumnData::DateTime2(None)
        | ColumnData::DateTimeOffset(None) => CellValue::Null,
        ColumnData::Bit(Some(value)) => CellValue::Bool(*value),
        ColumnData::U8(Some(value)) => CellValue::Int(i64::from(*value)),
        ColumnData::I16(Some(value)) => CellValue::Int(i64::from(*value)),
        ColumnData::I32(Some(value)) => CellValue::Int(i64::from(*value)),
        ColumnData::I64(Some(value)) => CellValue::Int(*value),
        // The driver decodes money as a float, so it's shown at money's fixed scale of 4.
        ColumnData::F32(Some(value))
            if matches!(kind, Some(ColumnType::Money | ColumnType::Money4)) =>
        {
            CellValue::Text(format!("{value:.4}"))
        }
        ColumnData::F64(Some(value))
            if matches!(kind, Some(ColumnType::Money | ColumnType::Money4)) =>
        {
            CellValue::Text(format!("{value:.4}"))
        }
        ColumnData::F32(Some(value)) => CellValue::from_float(f64::from(*value)),
        ColumnData::F64(Some(value)) => CellValue::from_float(*value),
        ColumnData::Numeric(Some(value)) => CellValue::Text(value.to_string()),
        ColumnData::String(Some(value)) => match kind {
            Some(
                ColumnType::Daten
                | ColumnType::Datetimen
                | ColumnType::Datetime
                | ColumnType::Datetime4
                | ColumnType::Datetime2
                | ColumnType::DatetimeOffsetn
                | ColumnType::Timen,
            ) => CellValue::DateTime(value.to_string()),
            _ => CellValue::Text(value.to_string()),
        },
        ColumnData::Guid(Some(value)) => CellValue::Text(value.to_string()),
        ColumnData::Binary(Some(value)) => CellValue::Bytes(value.to_vec()),
        ColumnData::Xml(Some(value)) => CellValue::Text(value.to_string()),
        ColumnData::DateTime(Some(value)) => CellValue::DateTime(format_legacy_datetime(
            value.days(),
            value.seconds_fragments(),
        )),
        ColumnData::SmallDateTime(Some(value)) => CellValue::DateTime(format_small_datetime(
            i32::from(value.days()),
            value.seconds_fragments(),
        )),
        ColumnData::Date(Some(value)) => {
            CellValue::DateTime(format_date(value.days() as i64 - 719_162))
        }
        ColumnData::Time(Some(value)) => {
            CellValue::DateTime(format_time(value.increments(), value.scale()))
        }
        ColumnData::DateTime2(Some(value)) => CellValue::DateTime(format!(
            "{} {}",
            format_date(value.date().days() as i64 - 719_162),
            format_time(value.time().increments(), value.time().scale())
        )),
        ColumnData::DateTimeOffset(Some(value)) => CellValue::DateTime(format_datetimeoffset(
            value.datetime2().date().days(),
            value.datetime2().time().increments(),
            value.datetime2().time().scale(),
            value.offset(),
        )),
    }
}

/// The wire carries the UTC date and time, so the offset is added back for the local time.
fn format_datetimeoffset(days: u32, increments: u64, scale: u8, offset: i16) -> String {
    let scale = scale.min(7);
    let unit = 10i128.pow(u32::from(scale));
    let per_day = 86_400 * unit;
    let total =
        i128::from(days) * per_day + i128::from(increments) + i128::from(offset) * 60 * unit;
    let local_days = total.div_euclid(per_day) as i64;
    let local_increments = total.rem_euclid(per_day) as u64;
    let sign = if offset < 0 { '-' } else { '+' };
    let minutes = offset.unsigned_abs();
    format!(
        "{} {} {sign}{:02}:{:02}",
        format_date(local_days - 719_162),
        format_time(local_increments, scale),
        minutes / 60,
        minutes % 60
    )
}

fn type_name(kind: ColumnType) -> &'static str {
    match kind {
        ColumnType::Bit | ColumnType::Bitn => "bit",
        ColumnType::Int1 => "tinyint",
        ColumnType::Int2 => "smallint",
        ColumnType::Int4 | ColumnType::Intn => "int",
        ColumnType::Int8 => "bigint",
        ColumnType::Float4 | ColumnType::Floatn => "real",
        ColumnType::Float8 => "float",
        ColumnType::Money => "money",
        ColumnType::Money4 => "smallmoney",
        ColumnType::Decimaln | ColumnType::Numericn => "decimal",
        ColumnType::Guid => "uniqueidentifier",
        ColumnType::Datetime | ColumnType::Datetimen => "datetime",
        ColumnType::Datetime4 => "smalldatetime",
        ColumnType::Datetime2 => "datetime2",
        ColumnType::DatetimeOffsetn => "datetimeoffset",
        ColumnType::Daten => "date",
        ColumnType::Timen => "time",
        ColumnType::BigChar | ColumnType::BigVarChar | ColumnType::Text => "varchar",
        ColumnType::NChar | ColumnType::NVarchar | ColumnType::NText => "nvarchar",
        ColumnType::BigBinary | ColumnType::BigVarBin | ColumnType::Image => "varbinary",
        ColumnType::Xml => "xml",
        ColumnType::Null | ColumnType::Udt | ColumnType::SSVariant => "sql_variant",
    }
}

/// Days since 1970-01-01, via Howard Hinnant's civil-from-days conversion.
fn format_date(unix_days: i64) -> String {
    let z = unix_days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

fn format_clock(seconds: u64, frac: u64, scale: u8) -> String {
    let hour = seconds / 3600;
    let minute = (seconds % 3600) / 60;
    let second = seconds % 60;
    if scale == 0 {
        format!("{hour:02}:{minute:02}:{second:02}")
    } else {
        format!(
            "{hour:02}:{minute:02}:{second:02}.{frac:0width$}",
            width = scale as usize
        )
    }
}

fn format_time(increments: u64, scale: u8) -> String {
    let scale = scale.min(7);
    let divisor = 10u64.pow(u32::from(scale));
    format_clock(increments / divisor, increments % divisor, scale)
}

/// `datetime`: days since 1900-01-01, and 1/300ths of a second since midnight.
fn format_legacy_datetime(days: i32, fragments: u32) -> String {
    let seconds = u64::from(fragments) / 300;
    let millis = (u64::from(fragments) % 300) * 10 / 3;
    format!(
        "{} {}",
        format_date(i64::from(days) - 25_567),
        format_clock(seconds, millis, 3)
    )
}

/// `smalldatetime`: days since 1900-01-01, and minutes since midnight.
fn format_small_datetime(days: i32, minutes: u16) -> String {
    format!(
        "{} {}",
        format_date(i64::from(days) - 25_567),
        format_clock(u64::from(minutes) * 60, 0, 0)
    )
}

pub fn literal(cell: &CellValue) -> String {
    match cell {
        CellValue::Null => "NULL".into(),
        CellValue::Bool(value) => i64::from(*value).to_string(),
        CellValue::Int(value) => value.to_string(),
        CellValue::Float(value) => value.to_string(),
        CellValue::Bytes(bytes) => format!("0x{}", super::hex(bytes)),
        CellValue::Text(text) | CellValue::Json(text) | CellValue::DateTime(text) => {
            n_literal(text)
        }
    }
}

/// Unicode literal. Backslashes are ordinary characters.
pub fn n_literal(value: &str) -> String {
    format!("N'{}'", value.replace('\'', "''"))
}

pub fn edit_literal(value: &super::EditValue) -> String {
    match value {
        super::EditValue::Null => "NULL".into(),
        super::EditValue::Bool(value) => i64::from(*value).to_string(),
        super::EditValue::Int(value) => value.to_string(),
        super::EditValue::Float(value) => value.to_string(),
        super::EditValue::Text(value) => n_literal(value),
        super::EditValue::Now(kind) => now_sql(*kind).into(),
    }
}

/// SQL Server 2022 has no `CURRENT_DATE` or `CURRENT_TIME`.
pub fn now_sql(kind: super::NowKind) -> &'static str {
    match kind {
        super::NowKind::Date => "CAST(SYSDATETIME() AS date)",
        super::NowKind::Time => "CAST(SYSDATETIME() AS time)",
        // Keeps the server's offset for datetimeoffset, and its local time for the others.
        super::NowKind::Datetime => "SYSDATETIMEOFFSET()",
    }
}

pub fn split_table(table: &str) -> (&str, &str) {
    match table.split_once('.') {
        Some((schema, name)) if !schema.is_empty() && !name.is_empty() => (schema, name),
        _ => ("dbo", table),
    }
}

/// `[database].[schema].[table]` without the database, for `sp_rename`.
/// A two-part name is already schema-qualified and stays as it is.
pub fn strip_database(qualified: &str) -> &str {
    if qualified.matches("].[").count() >= 2 {
        if let Some(index) = qualified.find("].[") {
            return &qualified[index + 2..];
        }
    }
    qualified
}

pub(crate) fn describe(err: tiberius::error::Error) -> String {
    if is_cancel(&err) {
        return "Query cancelled.".into();
    }
    if is_lost(&err) {
        return format!("{CONNECTION_LOST} ({err}).");
    }
    err.to_string()
}

fn is_cancel(err: &tiberius::error::Error) -> bool {
    if matches!(err.code(), Some(596 | 3621)) {
        return true;
    }
    let message = err.to_string().to_ascii_lowercase();
    message.contains("kill state")
        || message.contains("query was cancel")
        || message.contains("attention signal")
}

fn is_lost(err: &tiberius::error::Error) -> bool {
    match err {
        tiberius::error::Error::Io { .. } => true,
        _ => matches!(
            err.code(),
            Some(64 | 109 | 232 | 233 | 10053 | 10054 | 10060)
        ),
    }
}

async fn apply(client: &mut MssqlClient, statements: &[EditStatement]) -> Result<(), String> {
    client.begin_transaction().await.map_err(describe)?;
    let result = async {
        for statement in statements {
            if !statement.params.is_empty() {
                return Err("SQL Server edits are sent as text.".into());
            }
            let output = run(client, &statement.sql, 1, None).await?;
            if statement.expect_one_row {
                super::check_matched(statement, output.rows_affected)?;
            }
        }
        Ok(())
    }
    .await;
    match result {
        Ok(()) => client.commit_transaction().await.map_err(describe),
        Err(err) => {
            let _ = client.rollback_transaction().await;
            Err(err)
        }
    }
}

pub(crate) const TYPE_EXPR: &str = "CONCAT(ty.name, CASE \
    WHEN ty.name IN ('varchar', 'char', 'varbinary', 'binary') THEN CONCAT('(', \
        CASE WHEN c.max_length = -1 THEN 'max' ELSE CONVERT(varchar(10), c.max_length) END, ')') \
    WHEN ty.name IN ('nvarchar', 'nchar') THEN CONCAT('(', \
        CASE WHEN c.max_length = -1 THEN 'max' ELSE CONVERT(varchar(10), c.max_length / 2) END, ')') \
    WHEN ty.name IN ('decimal', 'numeric') THEN CONCAT('(', c.precision, ',', c.scale, ')') \
    WHEN ty.name IN ('datetime2', 'datetimeoffset', 'time') THEN CONCAT('(', c.scale, ')') \
    ELSE '' END)";

pub struct MssqlDialect;

impl Dialect for MssqlDialect {
    fn version_sql(&self) -> &'static str {
        "SELECT CONVERT(nvarchar(128), SERVERPROPERTY('ProductVersion'))"
    }

    fn namespaces_sql(&self) -> &'static str {
        "SELECT name FROM sys.databases WHERE source_database_id IS NULL ORDER BY name"
    }

    fn current_namespace_sql(&self) -> &'static str {
        "SELECT DB_NAME()"
    }

    fn namespace_label(&self) -> &'static str {
        "Database"
    }

    fn system_namespaces(&self) -> &'static [&'static str] {
        &["master", "tempdb", "model", "msdb"]
    }

    fn tables_sql(&self, namespace: &str) -> String {
        let db = quote_bracket(namespace);
        format!(
            "SELECT s.name + N'.' + o.name, CASE WHEN o.type = 'V' THEN 'view' ELSE 'table' END \
             FROM {db}.sys.objects o \
             JOIN {db}.sys.schemas s ON s.schema_id = o.schema_id \
             WHERE o.type IN ('U', 'V') AND s.name NOT IN (N'sys', N'INFORMATION_SCHEMA') \
             ORDER BY s.name, o.name"
        )
    }

    fn columns_sql(&self, namespace: &str, table: &str) -> String {
        let db = quote_bracket(namespace);
        let (schema, name) = split_table(table);
        format!(
            "SELECT c.name, {TYPE_EXPR}, CASE WHEN c.is_nullable = 1 THEN 'YES' ELSE 'NO' END, \
             OBJECT_DEFINITION(dc.object_id), \
             CASE WHEN pk.column_id IS NOT NULL THEN 1 ELSE 0 END, \
             CASE WHEN c.is_computed = 1 THEN 'computed' WHEN c.is_identity = 1 THEN 'identity' ELSE '' END \
             FROM {db}.sys.columns c \
             JOIN {db}.sys.objects o ON o.object_id = c.object_id \
             JOIN {db}.sys.schemas s ON s.schema_id = o.schema_id \
             JOIN {db}.sys.types ty ON ty.user_type_id = c.user_type_id \
             LEFT JOIN {db}.sys.default_constraints dc \
               ON dc.parent_object_id = c.object_id AND dc.parent_column_id = c.column_id \
             LEFT JOIN ( \
               SELECT ic.object_id, ic.column_id FROM {db}.sys.indexes i \
               JOIN {db}.sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id \
               WHERE i.is_primary_key = 1 \
             ) pk ON pk.object_id = c.object_id AND pk.column_id = c.column_id \
             WHERE s.name = {} AND o.name = {} \
             ORDER BY c.column_id",
            n_literal(schema),
            n_literal(name)
        )
    }

    fn indexes_sql(&self, namespace: &str, table: &str) -> String {
        index_sql(namespace, table, false)
    }

    fn index_definitions_sql(&self, namespace: &str, table: &str) -> String {
        index_sql(namespace, table, true)
    }

    fn schema_columns_sql(&self, namespace: &str) -> String {
        let db = quote_bracket(namespace);
        format!(
            "SELECT s.name + N'.' + o.name, c.name \
             FROM {db}.sys.columns c \
             JOIN {db}.sys.objects o ON o.object_id = c.object_id \
             JOIN {db}.sys.schemas s ON s.schema_id = o.schema_id \
             WHERE o.type IN ('U', 'V') AND s.name NOT IN (N'sys', N'INFORMATION_SCHEMA') \
             ORDER BY s.name, o.name, c.column_id"
        )
    }

    fn foreign_keys_sql(&self, namespace: &str, table: &str) -> String {
        let db = quote_bracket(namespace);
        let (schema, name) = split_table(table);
        format!(
            "SELECT fk.name, pc.name, rs.name, rt.name, rc.name \
             FROM {db}.sys.foreign_keys fk \
             JOIN {db}.sys.foreign_key_columns fkc ON fkc.constraint_object_id = fk.object_id \
             JOIN {db}.sys.tables pt ON pt.object_id = fk.parent_object_id \
             JOIN {db}.sys.schemas ps ON ps.schema_id = pt.schema_id \
             JOIN {db}.sys.columns pc ON pc.object_id = fkc.parent_object_id AND pc.column_id = fkc.parent_column_id \
             JOIN {db}.sys.tables rt ON rt.object_id = fk.referenced_object_id \
             JOIN {db}.sys.schemas rs ON rs.schema_id = rt.schema_id \
             JOIN {db}.sys.columns rc ON rc.object_id = fkc.referenced_object_id AND rc.column_id = fkc.referenced_column_id \
             WHERE ps.name = {} AND pt.name = {} \
             ORDER BY fk.name, fkc.constraint_column_id",
            n_literal(schema),
            n_literal(name)
        )
    }

    fn diagram_columns_sql(&self, namespace: &str) -> String {
        let db = quote_bracket(namespace);
        format!(
            "SELECT s.name + N'.' + o.name, c.name, {TYPE_EXPR}, \
             CASE WHEN pk.column_id IS NOT NULL THEN 1 ELSE 0 END \
             FROM {db}.sys.columns c \
             JOIN {db}.sys.objects o ON o.object_id = c.object_id \
             JOIN {db}.sys.schemas s ON s.schema_id = o.schema_id \
             JOIN {db}.sys.types ty ON ty.user_type_id = c.user_type_id \
             LEFT JOIN ( \
               SELECT ic.object_id, ic.column_id FROM {db}.sys.indexes i \
               JOIN {db}.sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id \
               WHERE i.is_primary_key = 1 \
             ) pk ON pk.object_id = c.object_id AND pk.column_id = c.column_id \
             WHERE o.type IN ('U', 'V') AND s.name NOT IN (N'sys', N'INFORMATION_SCHEMA') \
             ORDER BY s.name, o.name, c.column_id"
        )
    }

    fn schema_foreign_keys_sql(&self, namespace: &str) -> String {
        let db = quote_bracket(namespace);
        format!(
            "SELECT ps.name + N'.' + pt.name, fk.name, pc.name, rs.name, rt.name, rc.name \
             FROM {db}.sys.foreign_keys fk \
             JOIN {db}.sys.foreign_key_columns fkc ON fkc.constraint_object_id = fk.object_id \
             JOIN {db}.sys.tables pt ON pt.object_id = fk.parent_object_id \
             JOIN {db}.sys.schemas ps ON ps.schema_id = pt.schema_id \
             JOIN {db}.sys.columns pc ON pc.object_id = fkc.parent_object_id AND pc.column_id = fkc.parent_column_id \
             JOIN {db}.sys.tables rt ON rt.object_id = fk.referenced_object_id \
             JOIN {db}.sys.schemas rs ON rs.schema_id = rt.schema_id \
             JOIN {db}.sys.columns rc ON rc.object_id = fkc.referenced_object_id AND rc.column_id = fkc.referenced_column_id \
             WHERE ps.name NOT IN (N'sys', N'INFORMATION_SCHEMA') \
             ORDER BY ps.name, pt.name, fk.name, fkc.constraint_column_id"
        )
    }

    fn quote_ident(&self, ident: &str) -> String {
        quote_bracket(ident)
    }

    fn qualified(&self, namespace: &str, table: &str) -> String {
        let (schema, name) = split_table(table);
        let object = format!("{}.{}", quote_bracket(schema), quote_bracket(name));
        if namespace.is_empty() {
            object
        } else {
            format!("{}.{}", quote_bracket(namespace), object)
        }
    }

    fn limit_sql(&self, limit: u64, offset: u64, ordered: bool) -> String {
        let order = if ordered {
            ""
        } else {
            " ORDER BY (SELECT NULL)"
        };
        format!("{order} OFFSET {offset} ROWS FETCH NEXT {limit} ROWS ONLY")
    }

    fn use_namespace_sql(&self, namespace: &str) -> Option<String> {
        Some(format!("USE {}", quote_bracket(namespace)))
    }

    fn create_namespace_sql(&self, namespace: &str) -> Option<String> {
        Some(format!("CREATE DATABASE {}", quote_bracket(namespace)))
    }

    fn drop_namespace_sql(&self, namespace: &str) -> Option<String> {
        Some(format!("DROP DATABASE {}", quote_bracket(namespace)))
    }

    fn rename_namespace_sql(&self, from: &str, to: &str) -> Option<String> {
        Some(format!(
            "ALTER DATABASE {} MODIFY NAME = {}",
            quote_bracket(from),
            quote_bracket(to)
        ))
    }
}

fn index_sql(namespace: &str, table: &str, definition: bool) -> String {
    let db = quote_bracket(namespace);
    let (schema, name) = split_table(table);
    let extra = if definition {
        ", ISNULL(i.filter_definition, N''), i.type_desc, N'', \
         CASE WHEN i.is_primary_key = 1 OR i.is_unique_constraint = 1 THEN 1 ELSE 0 END"
    } else {
        ""
    };
    format!(
        "SELECT i.name, STRING_AGG(CONCAT(QUOTENAME(c.name), CASE WHEN ic.is_descending_key = 1 THEN N' DESC' ELSE N'' END), N', ') \
         WITHIN GROUP (ORDER BY ic.key_ordinal), i.is_unique, i.is_primary_key{extra} \
         FROM {db}.sys.indexes i \
         JOIN {db}.sys.objects o ON o.object_id = i.object_id \
         JOIN {db}.sys.schemas s ON s.schema_id = o.schema_id \
         JOIN {db}.sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id AND ic.is_included_column = 0 AND ic.key_ordinal > 0 \
         JOIN {db}.sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id \
         WHERE s.name = {} AND o.name = {} AND i.type > 0 AND i.name IS NOT NULL \
         GROUP BY i.name, i.is_unique, i.is_primary_key, i.is_unique_constraint, i.filter_definition, i.type_desc, i.index_id \
         ORDER BY i.is_primary_key DESC, i.name",
        n_literal(schema),
        n_literal(name)
    )
}

pub fn other_sessions_sql(database: &str) -> String {
    format!(
        "SELECT COUNT(*) FROM sys.dm_exec_sessions WHERE database_id = DB_ID({}) AND session_id <> @@SPID",
        n_literal(database)
    )
}

pub fn identity_columns_sql(namespace: &str, table: &str) -> String {
    let db = quote_bracket(namespace);
    let (schema, name) = split_table(table);
    format!(
        "SELECT c.name FROM {db}.sys.columns c \
         JOIN {db}.sys.objects o ON o.object_id = c.object_id \
         JOIN {db}.sys.schemas s ON s.schema_id = o.schema_id \
         WHERE s.name = {} AND o.name = {} AND c.is_identity = 1",
        n_literal(schema),
        n_literal(name)
    )
}

pub fn identity_columns(output: &RawOutput) -> Vec<String> {
    output
        .text_rows()
        .into_iter()
        .filter_map(|row| row.into_iter().next().flatten())
        .filter(|name| !name.is_empty())
        .collect()
}

pub fn column_definitions_sql(namespace: &str, table: &str) -> String {
    let db = quote_bracket(namespace);
    let (schema, name) = split_table(table);
    format!(
        "SELECT c.name, {TYPE_EXPR}, CASE WHEN c.is_nullable = 1 THEN 'YES' ELSE 'NO' END, \
         OBJECT_DEFINITION(dc.object_id), dc.name, c.is_computed, c.is_identity \
         FROM {db}.sys.columns c \
         JOIN {db}.sys.objects o ON o.object_id = c.object_id \
         JOIN {db}.sys.schemas s ON s.schema_id = o.schema_id \
         JOIN {db}.sys.types ty ON ty.user_type_id = c.user_type_id \
         LEFT JOIN {db}.sys.default_constraints dc \
           ON dc.parent_object_id = c.object_id AND dc.parent_column_id = c.column_id \
         WHERE s.name = {} AND o.name = {} \
         ORDER BY c.column_id",
        n_literal(schema),
        n_literal(name)
    )
}

pub fn column_definitions(output: &RawOutput) -> Vec<ColumnDefinition> {
    output
        .rows
        .iter()
        .map(|row| {
            let text = texts(row);
            ColumnDefinition {
                name: text_at(&text, 0),
                data_type: text_at(&text, 1),
                nullable: text_at(&text, 2).eq_ignore_ascii_case("YES"),
                default_value: text
                    .get(3)
                    .cloned()
                    .flatten()
                    .filter(|value| !value.is_empty()),
                default_constraint: text
                    .get(4)
                    .cloned()
                    .flatten()
                    .filter(|value| !value.is_empty()),
                computed: row.get(5).is_some_and(CellValue::is_truthy),
                identity: row.get(6).is_some_and(CellValue::is_truthy),
            }
        })
        .collect()
}

pub fn alter_column_statements(
    table: &str,
    change: &ColumnChange,
    current: &ColumnDefinition,
) -> Result<Vec<String>, String> {
    let old = quote_bracket(&current.name);
    if current.computed && !change.only_renames() {
        return Err(format!(
            "{} is a computed column, so Recon can only rename it.",
            current.name
        ));
    }
    if current.computed {
        return Ok(change
            .renamed()
            .map(|name| {
                sp_rename(
                    &format!("{}.{}", strip_database(table), old),
                    name,
                    "COLUMN",
                )
            })
            .into_iter()
            .collect());
    }
    let mut sql = Vec::new();
    let type_changes = change.data_type.is_some() || change.nullable.is_some();
    let next_default = match &change.default_value {
        Some(value) => value.clone(),
        None if type_changes => current.default_value.clone(),
        None => None,
    };
    let drop_default = current.default_constraint.is_some()
        && (change.default_value.is_some() || (type_changes && current.default_value.is_some()));
    if drop_default {
        if let Some(constraint) = &current.default_constraint {
            sql.push(format!(
                "ALTER TABLE {table} DROP CONSTRAINT {}",
                quote_bracket(constraint)
            ));
        }
    }
    if type_changes {
        let data_type = change
            .data_type
            .as_deref()
            .unwrap_or(&current.data_type)
            .trim();
        let nullable = change.nullable.unwrap_or(current.nullable);
        let null_sql = if nullable { "NULL" } else { "NOT NULL" };
        sql.push(format!(
            "ALTER TABLE {table} ALTER COLUMN {old} {data_type} {null_sql}"
        ));
    }
    if let Some(expr) = next_default
        .as_deref()
        .map(str::trim)
        .filter(|expr| !expr.is_empty())
    {
        if change.default_value.is_some() || type_changes {
            sql.push(format!("ALTER TABLE {table} ADD DEFAULT {expr} FOR {old}"));
        }
    }
    if let Some(name) = change.renamed() {
        sql.push(sp_rename(
            &format!("{}.{}", strip_database(table), old),
            name,
            "COLUMN",
        ));
    }
    Ok(sql)
}

pub fn sp_rename(object: &str, new_name: &str, kind: &str) -> String {
    format!(
        "EXEC sp_rename {}, {}, N'{kind}'",
        n_literal(object),
        n_literal(&quote_bracket(new_name))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_dates_from_day_counts() {
        assert_eq!(format_date(0), "1970-01-01");
        assert_eq!(format_date(-25_567), "1900-01-01");
        assert_eq!(format_legacy_datetime(0, 300), "1900-01-01 00:00:01.000");
        assert_eq!(format_small_datetime(0, 90), "1900-01-01 01:30:00");
        assert_eq!(format_time(3_661_000, 3), "01:01:01.000");
    }

    #[test]
    fn shows_datetimeoffset_in_its_own_offset() {
        // 2026-01-01 is day 739_616 since 0001-01-01. 15:00 UTC is 09:00 at -06:00.
        let days = 739_616;
        let utc = 15 * 3600 * 1000;
        assert_eq!(
            format_datetimeoffset(days, utc, 3, -360),
            "2026-01-01 09:00:00.000 -06:00"
        );
        // 23:30 UTC at +05:30 rolls into the next day.
        assert_eq!(
            format_datetimeoffset(days, (23 * 3600 + 1800) * 1000, 3, 330),
            "2026-01-02 05:00:00.000 +05:30"
        );
        assert_eq!(
            format_datetimeoffset(days, 0, 0, 0),
            "2026-01-01 00:00:00 +00:00"
        );
    }

    #[test]
    fn finds_views_and_routines_past_comments() {
        assert_eq!(defined_module("CREATE VIEW v AS SELECT 1"), Some("VIEW"));
        assert_eq!(
            defined_module("-- note\n/* more */ create or alter proc p AS SELECT 1"),
            Some("PROCEDURE")
        );
        assert_eq!(
            defined_module("ALTER FUNCTION f() RETURNS int"),
            Some("FUNCTION")
        );
        assert_eq!(
            defined_module("CREATE TRIGGER t ON x AFTER INSERT AS"),
            Some("TRIGGER")
        );
        assert_eq!(defined_module("CREATE TABLE t (id int)"), None);
        assert_eq!(defined_module("ALTER TABLE t ADD c int"), None);
        assert_eq!(defined_module("UPDATE t SET a = 1"), None);
    }

    #[test]
    fn uses_sql_server_functions_for_now() {
        use super::super::{EditValue, NowKind};
        assert_eq!(
            edit_literal(&EditValue::Now(NowKind::Date)),
            "CAST(SYSDATETIME() AS date)"
        );
        assert_eq!(
            edit_literal(&EditValue::Now(NowKind::Datetime)),
            "SYSDATETIMEOFFSET()"
        );
    }

    #[test]
    fn quotes_unicode_literals_and_splits_tables() {
        assert_eq!(n_literal("o'brien"), "N'o''brien'");
        assert_eq!(split_table("dbo.orders"), ("dbo", "orders"));
        assert_eq!(split_table("orders"), ("dbo", "orders"));
        assert_eq!(strip_database("[app].[dbo].[orders]"), "[dbo].[orders]");
        assert_eq!(strip_database("[dbo].[orders]"), "[dbo].[orders]");
    }

    #[test]
    fn alters_columns_and_renames_last() {
        let current = ColumnDefinition {
            name: "title".into(),
            data_type: "nvarchar(40)".into(),
            nullable: true,
            default_value: Some("(N'draft')".into()),
            default_constraint: Some("DF_posts_title".into()),
            computed: false,
            identity: false,
        };
        let change = serde_json::from_value(serde_json::json!({
            "column": "title",
            "name": "headline",
            "nullable": false,
        }))
        .unwrap();
        let sql = alter_column_statements("[app].[dbo].[posts]", &change, &current).unwrap();
        assert_eq!(
            sql,
            vec![
                "ALTER TABLE [app].[dbo].[posts] DROP CONSTRAINT [DF_posts_title]",
                "ALTER TABLE [app].[dbo].[posts] ALTER COLUMN [title] nvarchar(40) NOT NULL",
                "ALTER TABLE [app].[dbo].[posts] ADD DEFAULT (N'draft') FOR [title]",
                "EXEC sp_rename N'[dbo].[posts].[title]', N'[headline]', N'COLUMN'",
            ]
        );
        let computed = ColumnDefinition {
            computed: true,
            ..current
        };
        assert!(alter_column_statements("[posts]", &change, &computed).is_err());
    }

    #[test]
    fn pages_with_offset_fetch() {
        let dialect = MssqlDialect;
        assert_eq!(
            dialect.limit_sql(50, 100, false),
            " ORDER BY (SELECT NULL) OFFSET 100 ROWS FETCH NEXT 50 ROWS ONLY"
        );
        assert_eq!(
            dialect.limit_sql(50, 0, true),
            " OFFSET 0 ROWS FETCH NEXT 50 ROWS ONLY"
        );
        assert_eq!(
            dialect.qualified("app", "dbo.orders"),
            "[app].[dbo].[orders]"
        );
    }

    #[test]
    fn refuses_sql_server_before_2022_and_wraps_identity_insert() {
        assert_eq!(
            version_gate(15, "15.0.2000.5").unwrap_err(),
            "Recon supports SQL Server 2022 or newer. This server is 15.0.2000.5."
        );
        assert_eq!(version_gate(16, "16.0.1000.6").unwrap(), "16.0.1000.6");
        assert_eq!(
            identity_insert_sql("[dbo].[posts]", true),
            "SET IDENTITY_INSERT [dbo].[posts] ON"
        );
        assert_eq!(
            identity_insert_sql("[dbo].[posts]", false),
            "SET IDENTITY_INSERT [dbo].[posts] OFF"
        );
    }

    fn live_entry() -> Option<(crate::models::ConnectionEntry, Option<String>)> {
        let user = std::env::var("RECON_TEST_MSSQL_USER").ok()?;
        let entry = serde_json::from_value(serde_json::json!({
            "id": "live",
            "name": "live",
            "driver": "mssql",
            "host": std::env::var("RECON_TEST_MSSQL_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
            "port": 1433,
            "user": user,
            "database": "",
            "filePath": "",
            "sslMode": "prefer",
            "headerColor": "",
            "savePassword": false,
        }))
        .unwrap();
        Some((entry, std::env::var("RECON_TEST_MSSQL_PASSWORD").ok()))
    }

    async fn exec(conn: &mut super::Conn, sql: &str) -> Result<super::RawOutput, String> {
        conn.run(sql, 10, None).await
    }

    /// Runs against a real server: `RECON_TEST_MSSQL_USER=sa cargo test -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn live_server_round_trip() {
        let Some((entry, password)) = live_entry() else {
            return;
        };
        let version = test(&entry, password.as_deref()).await.unwrap();
        assert!(!version.is_empty());
        let mut opened = open(&entry, password.as_deref()).await.unwrap();
        assert!(opened.backend_id.is_some());
        exec(&mut opened.conn, "DROP DATABASE IF EXISTS recon_mssql_live")
            .await
            .ok();
        exec(
            &mut opened.conn,
            "DROP DATABASE IF EXISTS recon_mssql_live_dst",
        )
        .await
        .ok();
        exec(&mut opened.conn, "CREATE DATABASE recon_mssql_live")
            .await
            .unwrap();
        exec(&mut opened.conn, "USE [recon_mssql_live]")
            .await
            .unwrap();
        exec(
            &mut opened.conn,
            "CREATE TABLE dbo.posts (\
                id int IDENTITY(1,1) PRIMARY KEY, \
                title nvarchar(40) NOT NULL, \
                active bit NOT NULL, \
                stamp datetimeoffset NULL, \
                token uniqueidentifier NULL, \
                rev rowversion)",
        )
        .await
        .unwrap();
        exec(
            &mut opened.conn,
            "INSERT INTO dbo.posts (title, active, stamp) VALUES (N'o''brien', 1, '2026-01-01 09:00 -06:00')",
        )
        .await
        .unwrap();
        exec(
            &mut opened.conn,
            "CREATE VIEW dbo.active_posts AS SELECT id, title FROM dbo.posts WHERE active = 1",
        )
        .await
        .unwrap();
        exec(
            &mut opened.conn,
            "-- Keeps its body\nCREATE PROCEDURE dbo.touch AS\nBEGIN\n  UPDATE dbo.posts SET active = 1;\n  SELECT 1;\nEND",
        )
        .await
        .unwrap();
        let stamp = exec(&mut opened.conn, "SELECT stamp FROM dbo.posts")
            .await
            .unwrap();
        assert_eq!(
            stamp.rows[0][0],
            CellValue::DateTime("2026-01-01 09:00:00.0000000 -06:00".into())
        );
        let rows = exec(&mut opened.conn, "SELECT title, active, rev FROM dbo.posts")
            .await
            .unwrap();
        assert_eq!(rows.rows[0][0], CellValue::Text("o'brien".into()));
        assert_eq!(rows.rows[0][1], CellValue::Bool(true));
        assert!(matches!(rows.rows[0][2], CellValue::Bytes(_)));
        exec(
            &mut opened.conn,
            "UPDATE dbo.posts SET title = N'next' WHERE id = 1",
        )
        .await
        .unwrap();
        let tables = exec(
            &mut opened.conn,
            &MssqlDialect.tables_sql("recon_mssql_live"),
        )
        .await
        .unwrap();
        assert!(tables
            .text_rows()
            .iter()
            .any(|row| super::text_at(row, 0) == "dbo.posts"));

        let mut out = Vec::new();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let mut progress = |_: crate::db::dump::DumpProgress| {};
        let mut dump = crate::db::dump::Dump::new(&mut out, &cancel, &mut progress);
        crate::db::dump::run(
            &mut opened.conn,
            "SQL Server",
            "recon_mssql_live",
            &[
                crate::db::dump::DumpTable {
                    name: "dbo.posts".into(),
                    view: false,
                },
                crate::db::dump::DumpTable {
                    name: "dbo.active_posts".into(),
                    view: true,
                },
            ],
            crate::db::dump::DumpOptions {
                structure: true,
                data: true,
                drop_tables: true,
                routines: true,
            },
            &mut dump,
        )
        .await
        .unwrap();
        let sql = String::from_utf8(out).unwrap();
        assert!(sql.contains("IDENTITY_INSERT"), "{sql}");
        assert!(sql.contains("N'next'"), "{sql}");

        exec(&mut opened.conn, "CREATE DATABASE recon_mssql_live_dst")
            .await
            .unwrap();
        exec(&mut opened.conn, "USE [recon_mssql_live_dst]")
            .await
            .unwrap();
        let mut splitter = crate::db::sql_split::Splitter::new(crate::models::Driver::Mssql);
        let mut statements = Vec::new();
        for line in sql.split_inclusive('\n') {
            splitter.push_line(line, &mut statements);
        }
        splitter.finish(&mut statements);
        for statement in &statements {
            opened
                .conn
                .run(&statement.sql, 0, None)
                .await
                .unwrap_or_else(|err| panic!("{err}: {}", statement.sql));
        }
        let copied = exec(&mut opened.conn, "SELECT title, active FROM dbo.posts")
            .await
            .unwrap();
        assert_eq!(copied.rows[0][0], CellValue::Text("next".into()));
        assert_eq!(copied.rows[0][1], CellValue::Bool(true));
        let body = exec(
            &mut opened.conn,
            "SELECT OBJECT_DEFINITION(OBJECT_ID(N'dbo.touch')), OBJECT_ID(N'dbo.active_posts')",
        )
        .await
        .unwrap();
        let definition = super::text_at(&super::texts(&body.rows[0]), 0);
        assert!(!definition.contains("@@ROWCOUNT"), "{definition}");
        assert!(body.rows[0][1] != CellValue::Null);
        exec(&mut opened.conn, "USE [master]").await.ok();
        exec(&mut opened.conn, "DROP DATABASE recon_mssql_live")
            .await
            .ok();
        exec(&mut opened.conn, "DROP DATABASE recon_mssql_live_dst")
            .await
            .ok();
        opened.conn.close().await;
        opened.pool.close().await;
    }
}
