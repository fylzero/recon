use std::sync::atomic::AtomicBool;

use sqlx::mysql::{
    MySqlConnectOptions, MySqlConnection, MySqlPoolOptions, MySqlQueryResult, MySqlRow, MySqlSslMode,
};
use sqlx::{Connection, MySql, Row, TypeInfo, ValueRef};

use super::{
    quote_backtick, run_raw, with_timeout, CellValue, Conn, Dialect, Opened, Pool, RawOutput,
    CONNECT_TIMEOUT,
};
use crate::models::ConnectionEntry;

fn ssl_mode(mode: &str) -> MySqlSslMode {
    match mode {
        "disable" => MySqlSslMode::Disabled,
        "require" => MySqlSslMode::Required,
        _ => MySqlSslMode::Preferred,
    }
}

fn options(entry: &ConnectionEntry, password: Option<&str>) -> MySqlConnectOptions {
    let mut options = MySqlConnectOptions::new()
        .host(&entry.host)
        .port(entry.port)
        .username(&entry.user)
        .ssl_mode(ssl_mode(&entry.ssl_mode));
    if let Some(password) = password.filter(|value| !value.is_empty()) {
        options = options.password(password);
    }
    if !entry.database.is_empty() {
        options = options.database(&entry.database);
    }
    options
}

pub async fn open(entry: &ConnectionEntry, password: Option<&str>) -> Result<Opened, String> {
    let options = options(entry, password);
    let pool = with_timeout(
        MySqlPoolOptions::new()
            .max_connections(4)
            .acquire_timeout(CONNECT_TIMEOUT)
            .connect_with(options.clone()),
    )
    .await?;
    let mut conn = with_timeout(MySqlConnection::connect_with(&options)).await?;
    let backend_id = sqlx::query_scalar::<_, i64>("SELECT CAST(CONNECTION_ID() AS SIGNED)")
        .fetch_one(&mut conn)
        .await
        .ok();
    Ok(Opened {
        pool: Pool::MySql(pool),
        conn: Conn::MySql(conn),
        backend_id,
    })
}

pub async fn test(entry: &ConnectionEntry, password: Option<&str>) -> Result<String, String> {
    let mut conn = with_timeout(MySqlConnection::connect_with(&options(entry, password))).await?;
    let version = run(&mut conn, MysqlDialect.version_sql(), 1, None).await;
    let _ = conn.close().await;
    Ok(super::first_text(&version?))
}

fn affected(result: &MySqlQueryResult) -> u64 {
    result.rows_affected()
}

pub async fn run(
    conn: &mut MySqlConnection,
    sql: &str,
    limit: usize,
    cancel: Option<&AtomicBool>,
) -> Result<RawOutput, String> {
    run_raw::<MySql>(conn, sql, limit, cancel, cell, affected).await
}

fn is_integer(type_name: &str) -> bool {
    matches!(
        type_name.trim_end_matches(" UNSIGNED"),
        "BOOLEAN" | "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "BIGINT" | "YEAR"
    )
}

fn text_or_bytes(row: &MySqlRow, index: usize) -> CellValue {
    match row.try_get_unchecked::<String, _>(index) {
        Ok(text) => CellValue::Text(text),
        Err(_) => row
            .try_get_unchecked::<Vec<u8>, _>(index)
            .map(CellValue::Bytes)
            .unwrap_or_else(|_| CellValue::Text(String::new())),
    }
}

fn text_as(row: &MySqlRow, index: usize, wrap: fn(String) -> CellValue) -> CellValue {
    match row.try_get_unchecked::<String, _>(index) {
        Ok(text) => wrap(text),
        Err(_) => text_or_bytes(row, index),
    }
}

pub fn cell(row: &MySqlRow, index: usize) -> CellValue {
    let type_name = match row.try_get_raw(index) {
        Ok(raw) if raw.is_null() => return CellValue::Null,
        Ok(raw) => raw.type_info().name().to_string(),
        Err(_) => return CellValue::Text(String::new()),
    };
    match type_name.as_str() {
        "BIGINT UNSIGNED" => match row.try_get_unchecked::<u64, _>(index) {
            Ok(value) => i64::try_from(value)
                .map(CellValue::Int)
                .unwrap_or_else(|_| CellValue::Text(value.to_string())),
            Err(_) => text_or_bytes(row, index),
        },
        name if is_integer(name) => row
            .try_get_unchecked::<i64, _>(index)
            .map(CellValue::Int)
            .unwrap_or_else(|_| text_or_bytes(row, index)),
        "FLOAT" | "DOUBLE" => row
            .try_get_unchecked::<f64, _>(index)
            .map(CellValue::from_float)
            .unwrap_or_else(|_| text_or_bytes(row, index)),
        "JSON" => text_as(row, index, CellValue::Json),
        "DATE" | "TIME" | "DATETIME" | "TIMESTAMP" => text_as(row, index, CellValue::DateTime),
        "BINARY" | "VARBINARY" | "BLOB" | "TINYBLOB" | "MEDIUMBLOB" | "LONGBLOB" | "BIT"
        | "GEOMETRY" => row
            .try_get_unchecked::<Vec<u8>, _>(index)
            .map(CellValue::Bytes)
            .unwrap_or_else(|_| text_or_bytes(row, index)),
        _ => text_or_bytes(row, index),
    }
}

fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "''"))
}

pub struct MysqlDialect;

impl Dialect for MysqlDialect {
    fn version_sql(&self) -> &'static str {
        "SELECT VERSION()"
    }

    fn namespaces_sql(&self) -> &'static str {
        "SHOW DATABASES"
    }

    fn current_namespace_sql(&self) -> &'static str {
        "SELECT DATABASE()"
    }

    fn namespace_label(&self) -> &'static str {
        "Database"
    }

    fn system_namespaces(&self) -> &'static [&'static str] {
        &["information_schema", "mysql", "performance_schema", "sys"]
    }

    fn tables_sql(&self, namespace: &str) -> String {
        format!(
            "SELECT TABLE_NAME, CASE WHEN TABLE_TYPE = 'BASE TABLE' THEN 'table' ELSE 'view' END \
             FROM information_schema.TABLES WHERE TABLE_SCHEMA = {} ORDER BY TABLE_NAME",
            literal(namespace)
        )
    }

    fn columns_sql(&self, namespace: &str, table: &str) -> String {
        format!(
            "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_DEFAULT, \
             CASE WHEN COLUMN_KEY = 'PRI' THEN 1 ELSE 0 END, EXTRA \
             FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = {} AND TABLE_NAME = {} \
             ORDER BY ORDINAL_POSITION",
            literal(namespace),
            literal(table)
        )
    }

    fn indexes_sql(&self, namespace: &str, table: &str) -> String {
        format!(
            "SELECT INDEX_NAME, GROUP_CONCAT(COLUMN_NAME ORDER BY SEQ_IN_INDEX SEPARATOR ', '), \
             CASE WHEN MIN(NON_UNIQUE) = 0 THEN 1 ELSE 0 END, \
             CASE WHEN INDEX_NAME = 'PRIMARY' THEN 1 ELSE 0 END \
             FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = {} AND TABLE_NAME = {} \
             GROUP BY INDEX_NAME ORDER BY INDEX_NAME = 'PRIMARY' DESC, INDEX_NAME",
            literal(namespace),
            literal(table)
        )
    }

    fn schema_columns_sql(&self, namespace: &str) -> String {
        format!(
            "SELECT TABLE_NAME, COLUMN_NAME FROM information_schema.COLUMNS \
             WHERE TABLE_SCHEMA = {} ORDER BY TABLE_NAME, ORDINAL_POSITION",
            literal(namespace)
        )
    }

    fn quote_ident(&self, ident: &str) -> String {
        quote_backtick(ident)
    }

    fn use_namespace_sql(&self, namespace: &str) -> Option<String> {
        Some(format!("USE {}", quote_backtick(namespace)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_backslashes_in_literals() {
        assert_eq!(literal("a\\'b"), "'a\\\\''b'");
    }

    #[test]
    fn recognizes_integer_types() {
        assert!(is_integer("INT UNSIGNED"));
        assert!(is_integer("BOOLEAN"));
        assert!(!is_integer("DECIMAL"));
    }

    fn live_entry() -> Option<(ConnectionEntry, Option<String>)> {
        let user = std::env::var("RECON_TEST_MYSQL_USER").ok()?;
        let entry = serde_json::from_value(serde_json::json!({
            "id": "live",
            "name": "live",
            "driver": "mysql",
            "host": std::env::var("RECON_TEST_MYSQL_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
            "port": 3306,
            "user": user,
            "database": "",
            "filePath": "",
            "sslMode": "prefer",
            "headerColor": "",
            "savePassword": false,
        }))
        .unwrap();
        Some((entry, std::env::var("RECON_TEST_MYSQL_PASSWORD").ok()))
    }

    /// Runs against a real server: `RECON_TEST_MYSQL_USER=root cargo test -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn live_server_round_trip() {
        let Some((entry, password)) = live_entry() else {
            return;
        };
        let version = test(&entry, password.as_deref()).await.unwrap();
        assert!(!version.is_empty());

        let opened = open(&entry, password.as_deref()).await.unwrap();
        assert!(opened.backend_id.is_some());
        let Conn::MySql(mut conn) = opened.conn else {
            panic!("expected a MySQL connection");
        };
        for sql in [
            "DROP DATABASE IF EXISTS recon_live_test",
            "CREATE DATABASE recon_live_test",
            "USE recon_live_test",
            "CREATE TABLE things (id INT PRIMARY KEY AUTO_INCREMENT, big BIGINT UNSIGNED, \
             price DECIMAL(8,2), ratio DOUBLE, label VARCHAR(40), doc JSON, at DATETIME, \
             raw VARBINARY(8), flag TINYINT(1), note TEXT NULL, UNIQUE KEY label_idx (label))",
            "INSERT INTO things (big, price, ratio, label, doc, at, raw, flag) VALUES \
             (18446744073709551615, 12.50, 0.25, 'it''s', '{\"a\":1}', '2026-01-02 03:04:05', 0xCAFE, 1)",
        ] {
            run(&mut conn, sql, 0, None).await.unwrap();
        }

        let output = run(&mut conn, "SELECT * FROM things", 100, None).await.unwrap();
        let names: Vec<_> = output.columns.iter().map(|column| column.name.as_str()).collect();
        assert_eq!(names[..3], ["id", "big", "price"]);
        let row = &output.rows[0];
        assert!(matches!(row[0], CellValue::Int(1)));
        assert!(matches!(&row[1], CellValue::Text(text) if text == "18446744073709551615"));
        assert!(matches!(&row[2], CellValue::Text(text) if text == "12.50"));
        assert!(matches!(row[3], CellValue::Float(value) if (value - 0.25).abs() < 1e-9));
        assert!(matches!(&row[4], CellValue::Text(text) if text == "it's"));
        assert!(matches!(&row[5], CellValue::Json(text) if text.contains("\"a\"")));
        assert!(matches!(&row[6], CellValue::DateTime(text) if text == "2026-01-02 03:04:05"));
        assert!(matches!(&row[7], CellValue::Bytes(bytes) if bytes == &[0xCA, 0xFE]));
        assert!(matches!(row[8], CellValue::Int(1)));
        assert!(matches!(row[9], CellValue::Null));

        let empty = run(&mut conn, "SELECT id, label FROM things WHERE id < 0", 100, None)
            .await
            .unwrap();
        assert_eq!(empty.columns.len(), 2);
        assert!(empty.rows.is_empty());

        let affected = run(&mut conn, "UPDATE things SET note = 'x'", 0, None).await.unwrap();
        assert!(affected.columns.is_empty());
        assert_eq!(affected.rows_affected, 1);

        let tables = run(&mut conn, &MysqlDialect.tables_sql("recon_live_test"), 100, None)
            .await
            .unwrap();
        assert_eq!(tables.text_rows()[0][0].as_deref(), Some("things"));
        let columns = run(&mut conn, &MysqlDialect.columns_sql("recon_live_test", "things"), 100, None)
            .await
            .unwrap();
        assert_eq!(columns.rows.len(), 10);
        assert!(columns.rows[0][4].is_truthy());
        let indexes = run(&mut conn, &MysqlDialect.indexes_sql("recon_live_test", "things"), 100, None)
            .await
            .unwrap();
        let index_rows = indexes.text_rows();
        assert_eq!(index_rows[0][0].as_deref(), Some("PRIMARY"));
        assert_eq!(index_rows[1][1].as_deref(), Some("label"));
        assert!(indexes.rows[1][2].is_truthy());

        let error = run(&mut conn, "SELECT * FROM missing_table", 100, None).await.unwrap_err();
        assert!(error.contains("missing_table"), "{error}");

        let Pool::MySql(pool) = opened.pool else {
            panic!("expected a MySQL pool");
        };
        let backend = opened.backend_id.unwrap();
        let killer = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            let mut other = pool.acquire().await.unwrap();
            run(&mut other, &format!("KILL QUERY {backend}"), 0, None).await.unwrap();
            pool
        });
        let started = std::time::Instant::now();
        let slept = run(&mut conn, "SELECT SLEEP(10)", 1, None).await;
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "{slept:?}");
        let pool = killer.await.unwrap();

        run(&mut conn, "DROP DATABASE recon_live_test", 0, None).await.unwrap();
        let _ = conn.close().await;
        pool.close().await;
    }
}
