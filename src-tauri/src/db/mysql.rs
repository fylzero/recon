use std::sync::atomic::AtomicBool;

use sqlx::mysql::{
    MySqlConnectOptions, MySqlConnection, MySqlPoolOptions, MySqlQueryResult, MySqlRow, MySqlSslMode,
};
use sqlx::{Connection, MySql, Row, TypeInfo, ValueRef};

use super::{
    quote_backtick, run_raw, text_at, with_timeout, CellValue, ColumnChange, Conn, Dialect, Opened,
    Pool, RawOutput, CONNECT_TIMEOUT,
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

/// Counts the objects `RENAME TABLE` can't carry into another database.
pub fn rename_blockers_sql(database: &str) -> String {
    let name = literal(database);
    format!(
        "SELECT (SELECT COUNT(*) FROM information_schema.VIEWS WHERE TABLE_SCHEMA = {name}) \
         + (SELECT COUNT(*) FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA = {name}) \
         + (SELECT COUNT(*) FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA = {name}) \
         + (SELECT COUNT(*) FROM information_schema.EVENTS WHERE EVENT_SCHEMA = {name})"
    )
}

pub fn charset_sql(database: &str) -> String {
    format!(
        "SELECT DEFAULT_CHARACTER_SET_NAME, DEFAULT_COLLATION_NAME \
         FROM information_schema.SCHEMATA WHERE SCHEMA_NAME = {}",
        literal(database)
    )
}

pub fn base_tables_sql(database: &str) -> String {
    format!(
        "SELECT TABLE_NAME FROM information_schema.TABLES \
         WHERE TABLE_SCHEMA = {} AND TABLE_TYPE = 'BASE TABLE' ORDER BY TABLE_NAME",
        literal(database)
    )
}

pub fn create_like_sql(database: &str, charset: &str, collation: &str) -> String {
    let mut sql = format!("CREATE DATABASE {}", quote_backtick(database));
    if !charset.is_empty() {
        sql.push_str(&format!(" CHARACTER SET {}", literal(charset)));
    }
    if !collation.is_empty() {
        sql.push_str(&format!(" COLLATE {}", literal(collation)));
    }
    sql
}

pub fn move_tables_sql(from: &str, to: &str, tables: &[String]) -> String {
    let moves: Vec<String> = tables
        .iter()
        .map(|table| {
            format!(
                "{}.{} TO {}.{}",
                quote_backtick(from),
                quote_backtick(table),
                quote_backtick(to),
                quote_backtick(table)
            )
        })
        .collect();
    format!("RENAME TABLE {}", moves.join(", "))
}

/**
 * Column defaults as SQL expressions that can be written back into a column
 * definition. MySQL stores literals unquoted and expressions without their
 * parentheses; MariaDB already stores SQL, with NULL spelled as the text NULL.
 */
const DEFAULT_EXPR: &str = "CASE \
    WHEN COLUMN_DEFAULT IS NULL THEN NULL \
    WHEN VERSION() LIKE '%MariaDB%' THEN NULLIF(COLUMN_DEFAULT, 'NULL') \
    WHEN DATA_TYPE IN ('timestamp', 'datetime') AND UPPER(COLUMN_DEFAULT) LIKE 'CURRENT\\_TIMESTAMP%' \
        THEN COLUMN_DEFAULT \
    WHEN EXTRA LIKE '%DEFAULT\\_GENERATED%' THEN CONCAT('(', COLUMN_DEFAULT, ')') \
    WHEN DATA_TYPE IN ('tinyint', 'smallint', 'mediumint', 'int', 'bigint', 'decimal', 'float', 'double', 'year') \
        OR COLUMN_DEFAULT REGEXP '^b''[01]*''$' THEN COLUMN_DEFAULT \
    ELSE QUOTE(COLUMN_DEFAULT) END";

/**
 * An index's column list as SQL, with prefix lengths and DESC. Expression
 * parts have no COLUMN_NAME, so they drop out and leave the list empty.
 */
const INDEX_COLUMNS_EXPR: &str = "GROUP_CONCAT(CONCAT(COLUMN_NAME, \
    IF(SUB_PART IS NULL, '', CONCAT('(', SUB_PART, ')')), \
    IF(COLLATION = 'D', ' DESC', '')) ORDER BY SEQ_IN_INDEX SEPARATOR ', ')";

#[derive(Debug, Clone, PartialEq)]
pub struct ColumnDefinition {
    pub name: String,
    pub column_type: String,
    pub nullable: bool,
    pub default_value: Option<String>,
    pub extra: String,
    pub comment: String,
    pub collation: Option<String>,
    pub generated: bool,
}

pub fn column_definitions_sql(namespace: &str, table: &str) -> String {
    format!(
        "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, {DEFAULT_EXPR}, EXTRA, COLUMN_COMMENT, \
         COLLATION_NAME, GENERATION_EXPRESSION \
         FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = {} AND TABLE_NAME = {} \
         ORDER BY ORDINAL_POSITION",
        literal(namespace),
        literal(table)
    )
}

pub fn column_definitions(output: &RawOutput) -> Vec<ColumnDefinition> {
    output
        .text_rows()
        .into_iter()
        .map(|row| {
            let extra = text_at(&row, 4);
            let upper = extra.to_ascii_uppercase();
            ColumnDefinition {
                name: text_at(&row, 0),
                column_type: text_at(&row, 1),
                nullable: text_at(&row, 2).eq_ignore_ascii_case("YES"),
                default_value: row.get(3).cloned().flatten(),
                comment: text_at(&row, 5),
                collation: row.get(6).cloned().flatten().filter(|value| !value.is_empty()),
                generated: !text_at(&row, 7).is_empty()
                    || upper.contains("VIRTUAL GENERATED")
                    || upper.contains("STORED GENERATED"),
                extra,
            }
        })
        .collect()
}

fn is_textual(column_type: &str) -> bool {
    let lower = column_type.trim().to_ascii_lowercase();
    ["char", "varchar", "tinytext", "text", "mediumtext", "longtext", "enum(", "set("]
        .iter()
        .any(|prefix| lower.starts_with(prefix))
}

/**
 * Keeps attributes the grid doesn't show (collation, auto_increment,
 * ON UPDATE, INVISIBLE, comment) so editing one field never drops them.
 */
pub fn change_column_sql(table: &str, change: &ColumnChange, current: &ColumnDefinition) -> Result<String, String> {
    let old = quote_backtick(&current.name);
    let new = quote_backtick(change.name.as_deref().unwrap_or(&current.name));
    if current.generated {
        if !change.only_renames() {
            return Err(format!("{} is a generated column, so Recon can only rename it.", current.name));
        }
        return Ok(format!("ALTER TABLE {table} RENAME COLUMN {old} TO {new}"));
    }
    let column_type = change.data_type.as_deref().unwrap_or(&current.column_type).trim();
    let mut definition = column_type.to_string();
    if let Some(collation) = current.collation.as_deref().filter(|_| is_textual(column_type)) {
        definition.push_str(&format!(" COLLATE {collation}"));
    }
    let nullable = change.nullable.unwrap_or(current.nullable);
    definition.push_str(if nullable { " NULL" } else { " NOT NULL" });
    let default_value = match &change.default_value {
        Some(value) => value.as_deref(),
        None => current.default_value.as_deref(),
    };
    if let Some(expr) = default_value {
        definition.push_str(&format!(" DEFAULT {}", expr.trim()));
    }
    let extra = current
        .extra
        .split_whitespace()
        .filter(|word| !word.eq_ignore_ascii_case("DEFAULT_GENERATED"))
        .collect::<Vec<_>>()
        .join(" ");
    if !extra.is_empty() {
        definition.push(' ');
        definition.push_str(&extra);
    }
    if !current.comment.is_empty() {
        definition.push_str(&format!(" COMMENT {}", literal(&current.comment)));
    }
    Ok(format!("ALTER TABLE {table} CHANGE COLUMN {old} {new} {definition}"))
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
            "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, {DEFAULT_EXPR}, \
             CASE WHEN COLUMN_KEY = 'PRI' THEN 1 ELSE 0 END, EXTRA \
             FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = {} AND TABLE_NAME = {} \
             ORDER BY ORDINAL_POSITION",
            literal(namespace),
            literal(table)
        )
    }

    fn indexes_sql(&self, namespace: &str, table: &str) -> String {
        format!(
            "SELECT INDEX_NAME, {INDEX_COLUMNS_EXPR}, \
             CASE WHEN MIN(NON_UNIQUE) = 0 THEN 1 ELSE 0 END, \
             CASE WHEN INDEX_NAME = 'PRIMARY' THEN 1 ELSE 0 END \
             FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = {} AND TABLE_NAME = {} \
             GROUP BY INDEX_NAME ORDER BY INDEX_NAME = 'PRIMARY' DESC, INDEX_NAME",
            literal(namespace),
            literal(table)
        )
    }

    fn index_definitions_sql(&self, namespace: &str, table: &str) -> String {
        format!(
            "SELECT INDEX_NAME, {INDEX_COLUMNS_EXPR}, \
             CASE WHEN MIN(NON_UNIQUE) = 0 THEN 1 ELSE 0 END, \
             CASE WHEN INDEX_NAME = 'PRIMARY' THEN 1 ELSE 0 END, \
             '', MAX(INDEX_TYPE), MAX(INDEX_COMMENT), 0 \
             FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = {} AND TABLE_NAME = {} \
             GROUP BY INDEX_NAME",
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

    fn create_namespace_sql(&self, namespace: &str) -> Option<String> {
        Some(format!("CREATE DATABASE {}", quote_backtick(namespace)))
    }

    fn drop_namespace_sql(&self, namespace: &str) -> Option<String> {
        Some(format!("DROP DATABASE {}", quote_backtick(namespace)))
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
    fn builds_database_rename_statements() {
        let tables = vec!["users".to_string(), "we`ird".to_string()];
        assert_eq!(
            move_tables_sql("old", "new", &tables),
            "RENAME TABLE `old`.`users` TO `new`.`users`, `old`.`we``ird` TO `new`.`we``ird`"
        );
        assert_eq!(
            create_like_sql("new", "utf8mb4", "utf8mb4_0900_ai_ci"),
            "CREATE DATABASE `new` CHARACTER SET 'utf8mb4' COLLATE 'utf8mb4_0900_ai_ci'"
        );
        assert_eq!(create_like_sql("new", "", ""), "CREATE DATABASE `new`");
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

        let table = MysqlDialect.qualified("recon_live_test", "things");
        let update = |id: i64, label: &str| -> super::super::RowUpdate {
            serde_json::from_value(serde_json::json!({
                "key": [{ "column": "id", "value": id }],
                "changes": [
                    { "column": "label", "value": label },
                    { "column": "flag", "value": false },
                    { "column": "ratio", "value": "0.25" },
                ],
            }))
            .unwrap()
        };
        let statement = |id: i64, label: &str| {
            super::super::update_statement(crate::models::Driver::Mysql, &table, &update(id, label)).unwrap()
        };
        let missing = opened
            .pool
            .apply(&[statement(1, "rolled back"), statement(404, "nope")])
            .await
            .unwrap_err();
        assert!(missing.contains("id = 404"), "{missing}");
        opened.pool.apply(&[statement(1, "a\\b 'c'")]).await.unwrap();
        opened.pool.apply(&[statement(1, "a\\b 'c'")]).await.unwrap();
        let saved = run(&mut conn, "SELECT label, flag, ratio FROM things WHERE id = 1", 1, None)
            .await
            .unwrap();
        assert!(matches!(&saved.rows[0][0], CellValue::Text(text) if text == "a\\b 'c'"));
        assert!(matches!(saved.rows[0][1], CellValue::Int(0)));

        run(
            &mut conn,
            "CREATE TABLE shapes (id INT AUTO_INCREMENT PRIMARY KEY, \
             title VARCHAR(50) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT 'it''s' COMMENT 'a ''note''', \
             u VARCHAR(36) DEFAULT (uuid()), \
             created TIMESTAMP NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP, \
             n INT DEFAULT 5, g INT AS (n * 2) VIRTUAL, \
             KEY by_title (title(10), n DESC) COMMENT 'short ''titles''', FULLTEXT KEY words (title))",
            0,
            None,
        )
        .await
        .unwrap();
        let definitions_sql = column_definitions_sql("recon_live_test", "shapes");
        let before = column_definitions(&run(&mut conn, &definitions_sql, 100, None).await.unwrap());
        let shapes = MysqlDialect.qualified("recon_live_test", "shapes");
        let mut statements = Vec::new();
        for change in [
            serde_json::json!({ "column": "id", "name": "shape_id" }),
            serde_json::json!({ "column": "title", "nullable": true }),
            serde_json::json!({ "column": "u", "name": "uid" }),
            serde_json::json!({ "column": "created", "dataType": "datetime" }),
            serde_json::json!({ "column": "g", "name": "double_id" }),
        ] {
            let change: ColumnChange = serde_json::from_value(change).unwrap();
            let current = before.iter().find(|column| column.name == change.column);
            statements.extend(
                super::super::alter_statements(crate::models::Driver::Mysql, &shapes, &change, current).unwrap(),
            );
        }
        opened.pool.apply(&statements).await.unwrap();
        let after = column_definitions(&run(&mut conn, &definitions_sql, 100, None).await.unwrap());
        let names: Vec<_> = after.iter().map(|column| column.name.as_str()).collect();
        assert_eq!(names, ["shape_id", "title", "uid", "created", "n", "double_id"]);
        assert!(after[0].extra.contains("auto_increment"));
        assert!(after[1].nullable);
        assert_eq!(after[1].collation.as_deref(), Some("utf8mb4_unicode_ci"));
        assert_eq!(after[1].comment, "a 'note'");
        assert_eq!(after[1].default_value, before[1].default_value);
        assert_eq!(after[2].default_value.as_deref(), Some("(uuid())"));
        assert_eq!(after[3].column_type, "datetime");
        assert_eq!(after[3].default_value.as_deref(), Some("CURRENT_TIMESTAMP"));
        assert!(after[3].extra.to_ascii_lowercase().contains("on update"));
        assert_eq!(after[4].default_value.as_deref(), Some("5"));
        assert!(after[5].generated);

        let indexes_sql = MysqlDialect.index_definitions_sql("recon_live_test", "shapes");
        let driver = crate::models::Driver::Mysql;
        let before = super::super::index_definitions(driver, &run(&mut conn, &indexes_sql, 100, None).await.unwrap());
        let mut statements = Vec::new();
        for change in [
            serde_json::json!({ "index": "by_title", "name": "title_n", "unique": true }),
            serde_json::json!({ "index": "words", "name": "title_words" }),
        ] {
            let change: super::super::IndexChange = serde_json::from_value(change).unwrap();
            let current = before.iter().find(|index| index.name == change.index);
            statements.extend(
                super::super::index_statements(driver, "recon_live_test", &shapes, &change, current).unwrap(),
            );
        }
        opened.pool.apply(&statements).await.unwrap();
        let after = super::super::index_definitions(driver, &run(&mut conn, &indexes_sql, 100, None).await.unwrap());
        let find = |name: &str| after.iter().find(|index| index.name == name).unwrap();
        assert_eq!(find("title_n").columns, "title(10), n DESC");
        assert!(find("title_n").unique);
        assert_eq!(find("title_n").comment, "short 'titles'");
        assert_eq!(find("title_words").index_type, "FULLTEXT");

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
