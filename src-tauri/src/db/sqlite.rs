use std::sync::atomic::AtomicBool;

use sqlx::sqlite::{
    SqliteConnectOptions, SqliteConnection, SqlitePoolOptions, SqliteQueryResult, SqliteRow,
};
use sqlx::{Connection, Row, Sqlite, TypeInfo, ValueRef};

use super::{
    quote_double, quote_literal, run_raw, with_timeout, CellValue, Conn, Dialect, Opened, Pool,
    RawOutput, CONNECT_TIMEOUT,
};
use crate::models::ConnectionEntry;

fn options(path: &str, create: bool) -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(create)
}

pub async fn open(entry: &ConnectionEntry) -> Result<Opened, String> {
    let options = options(&entry.file_path, false);
    let pool = with_timeout(
        SqlitePoolOptions::new()
            .max_connections(4)
            .acquire_timeout(CONNECT_TIMEOUT)
            .connect_with(options.clone()),
    )
    .await?;
    let conn = with_timeout(SqliteConnection::connect_with(&options)).await?;
    Ok(Opened {
        pool: Pool::Sqlite(pool),
        conn: Conn::Sqlite(conn),
        backend_id: None,
    })
}

pub async fn test(entry: &ConnectionEntry) -> Result<String, String> {
    if !std::path::Path::new(&entry.file_path).is_file() {
        return Err(format!("{} does not exist.", entry.file_path));
    }
    let mut conn = with_timeout(SqliteConnection::connect_with(&options(&entry.file_path, false))).await?;
    let version = run(&mut conn, SqliteDialect.version_sql(), 1, None).await;
    let _ = conn.close().await;
    Ok(super::first_text(&version?))
}

pub async fn create(path: &str) -> Result<(), String> {
    let conn = with_timeout(SqliteConnection::connect_with(&options(path, true))).await?;
    conn.close().await.map_err(super::describe_error)
}

fn affected(result: &SqliteQueryResult) -> u64 {
    result.rows_affected()
}

pub async fn run(
    conn: &mut SqliteConnection,
    sql: &str,
    limit: usize,
    cancel: Option<&AtomicBool>,
) -> Result<RawOutput, String> {
    run_raw::<Sqlite>(conn, sql, limit, cancel, cell, affected).await
}

pub fn cell(row: &SqliteRow, index: usize) -> CellValue {
    let type_name = match row.try_get_raw(index) {
        Ok(raw) if raw.is_null() => return CellValue::Null,
        Ok(raw) => raw.type_info().name().to_ascii_uppercase(),
        Err(_) => return CellValue::Text(String::new()),
    };
    let typed = match type_name.as_str() {
        "NULL" => Some(CellValue::Null),
        "INTEGER" | "INT" | "BIGINT" | "BOOLEAN" => {
            row.try_get_unchecked::<i64, _>(index).ok().map(CellValue::Int)
        }
        "REAL" | "FLOAT" | "DOUBLE" | "NUMERIC" => row
            .try_get_unchecked::<f64, _>(index)
            .ok()
            .map(CellValue::from_float),
        "BLOB" => row
            .try_get_unchecked::<Vec<u8>, _>(index)
            .ok()
            .map(CellValue::Bytes),
        _ => None,
    };
    typed.unwrap_or_else(|| match row.try_get_unchecked::<String, _>(index) {
        Ok(text) => CellValue::Text(text),
        Err(_) => row
            .try_get_unchecked::<Vec<u8>, _>(index)
            .map(CellValue::Bytes)
            .unwrap_or_else(|_| CellValue::Text(String::new())),
    })
}

pub struct SqliteDialect;

impl Dialect for SqliteDialect {
    fn version_sql(&self) -> &'static str {
        "SELECT sqlite_version()"
    }

    fn namespaces_sql(&self) -> &'static str {
        "SELECT name FROM pragma_database_list ORDER BY seq"
    }

    fn current_namespace_sql(&self) -> &'static str {
        "SELECT 'main'"
    }

    fn namespace_label(&self) -> &'static str {
        "Database"
    }

    fn system_namespaces(&self) -> &'static [&'static str] {
        &["temp"]
    }

    fn tables_sql(&self, namespace: &str) -> String {
        format!(
            "SELECT name, type FROM {}.sqlite_master \
             WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY name",
            quote_double(namespace)
        )
    }

    /** A lone INTEGER PRIMARY KEY aliases the rowid, so SQLite fills it in when left out. */
    fn columns_sql(&self, namespace: &str, table: &str) -> String {
        let (table, schema) = (quote_literal(table), quote_literal(namespace));
        format!(
            "SELECT p.name, p.type, CASE WHEN p.\"notnull\" THEN 'NO' ELSE 'YES' END, p.dflt_value, p.pk > 0, \
             CASE WHEN p.pk = 1 AND upper(p.type) = 'INTEGER' \
                  AND (SELECT count(*) FROM pragma_table_info({table}, {schema}) WHERE pk > 0) = 1 \
                  AND NOT EXISTS (SELECT 1 FROM {}.sqlite_master AS m WHERE m.type = 'table' \
                                  AND m.name = {table} AND upper(m.sql) LIKE '%WITHOUT ROWID%') \
             THEN 'auto_increment' ELSE '' END \
             FROM pragma_table_info({table}, {schema}) AS p ORDER BY p.cid",
            quote_double(namespace)
        )
    }

    fn indexes_sql(&self, namespace: &str, table: &str) -> String {
        let schema = quote_literal(namespace);
        format!(
            "SELECT il.name, \
             (SELECT group_concat(ii.name, ', ') FROM pragma_index_info(il.name, {schema}) AS ii), \
             il.\"unique\", il.origin = 'pk' \
             FROM pragma_index_list({}, {schema}) AS il ORDER BY il.origin = 'pk' DESC, il.name",
            quote_literal(table)
        )
    }

    fn index_definitions_sql(&self, namespace: &str, table: &str) -> String {
        let schema = quote_literal(namespace);
        format!(
            "SELECT il.name, \
             (SELECT group_concat(ii.name, ', ') FROM pragma_index_info(il.name, {schema}) AS ii), \
             il.\"unique\", il.origin = 'pk', m.sql, '', '', il.origin <> 'c' \
             FROM pragma_index_list({}, {schema}) AS il \
             LEFT JOIN {}.sqlite_master AS m ON m.type = 'index' AND m.name = il.name",
            quote_literal(table),
            quote_double(namespace)
        )
    }

    fn schema_columns_sql(&self, namespace: &str) -> String {
        format!(
            "SELECT m.name, p.name FROM {}.sqlite_master AS m \
             JOIN pragma_table_info(m.name, {}) AS p \
             WHERE m.type IN ('table', 'view') AND m.name NOT LIKE 'sqlite\\_%' ESCAPE '\\' \
             ORDER BY m.name, p.cid",
            quote_double(namespace),
            quote_literal(namespace)
        )
    }

    /** A key written as `REFERENCES t` has no `to` column and points at t's primary key. */
    fn foreign_keys_sql(&self, namespace: &str, table: &str) -> String {
        let schema = quote_literal(namespace);
        format!(
            "SELECT fk.id, fk.\"from\", {schema}, fk.\"table\", \
             COALESCE(fk.\"to\", (SELECT p.name FROM pragma_table_info(fk.\"table\", {schema}) AS p \
                                  WHERE p.pk = fk.seq + 1)) \
             FROM pragma_foreign_key_list({}, {schema}) AS fk ORDER BY fk.id, fk.seq",
            quote_literal(table)
        )
    }

    fn quote_ident(&self, ident: &str) -> String {
        quote_double(ident)
    }

    fn use_namespace_sql(&self, _namespace: &str) -> Option<String> {
        None
    }

    fn create_namespace_sql(&self, _namespace: &str) -> Option<String> {
        None
    }

    fn drop_namespace_sql(&self, _namespace: &str) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{texts, CellValue};

    async fn memory() -> SqliteConnection {
        SqliteConnection::connect("sqlite::memory:").await.unwrap()
    }

    #[tokio::test]
    async fn runs_queries_and_decodes_values() {
        let mut conn = memory().await;
        run(
            &mut conn,
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, score REAL, avatar BLOB)",
            0,
            None,
        )
        .await
        .unwrap();
        let insert = run(
            &mut conn,
            "INSERT INTO users (name, score, avatar) VALUES ('ada', 9.5, x'cafe'), ('bob', NULL, NULL)",
            0,
            None,
        )
        .await
        .unwrap();
        assert_eq!(insert.rows_affected, 2);
        assert!(insert.columns.is_empty());

        let select = run(&mut conn, "SELECT * FROM users ORDER BY id", 100, None).await.unwrap();
        assert_eq!(select.columns.len(), 4);
        assert_eq!(select.columns[1].name, "name");
        assert_eq!(
            select.rows[0],
            vec![
                CellValue::Int(1),
                CellValue::Text("ada".into()),
                CellValue::Float(9.5),
                CellValue::Bytes(vec![0xca, 0xfe]),
            ]
        );
        assert_eq!(select.rows[1][2], CellValue::Null);

        let limited = run(&mut conn, "SELECT * FROM users", 1, None).await.unwrap();
        assert_eq!(limited.rows.len(), 1);
        assert!(limited.truncated);

        let empty = run(&mut conn, "SELECT id, name FROM users WHERE 0", 100, None).await.unwrap();
        assert!(empty.rows.is_empty());
        assert_eq!(empty.columns.len(), 2);
    }

    #[tokio::test]
    async fn reads_schema_through_the_dialect() {
        let mut conn = memory().await;
        run(
            &mut conn,
            "CREATE TABLE posts (id INTEGER PRIMARY KEY, title TEXT NOT NULL DEFAULT 'x', slug TEXT UNIQUE)",
            0,
            None,
        )
        .await
        .unwrap();
        run(&mut conn, "CREATE VIEW recent AS SELECT id FROM posts", 0, None).await.unwrap();

        let dialect = SqliteDialect;
        let tables = run(&mut conn, &dialect.tables_sql("main"), 100, None).await.unwrap();
        let names: Vec<_> = tables.rows.iter().map(|row| texts(row)).collect();
        assert_eq!(names[0], vec![Some("posts".into()), Some("table".into())]);
        assert_eq!(names[1], vec![Some("recent".into()), Some("view".into())]);

        let columns = run(&mut conn, &dialect.columns_sql("main", "posts"), 100, None).await.unwrap();
        let title = texts(&columns.rows[1]);
        assert_eq!(title[0].as_deref(), Some("title"));
        assert_eq!(title[2].as_deref(), Some("NO"));
        assert_eq!(title[3].as_deref(), Some("'x'"));
        assert!(columns.rows[0][4].is_truthy());
        assert_eq!(texts(&columns.rows[0])[5].as_deref(), Some("auto_increment"));
        assert_eq!(title[5].as_deref(), Some(""));

        run(&mut conn, "CREATE TABLE tags (id INTEGER PRIMARY KEY, name TEXT) WITHOUT ROWID", 0, None)
            .await
            .unwrap();
        run(&mut conn, "CREATE TABLE pairs (a INTEGER, b INTEGER, PRIMARY KEY (a, b))", 0, None)
            .await
            .unwrap();
        for table in ["tags", "pairs"] {
            let columns = run(&mut conn, &dialect.columns_sql("main", table), 100, None).await.unwrap();
            assert_eq!(texts(&columns.rows[0])[5].as_deref(), Some(""), "{table}");
        }

        let indexes = run(&mut conn, &dialect.indexes_sql("main", "posts"), 100, None).await.unwrap();
        let slug = indexes
            .rows
            .iter()
            .map(|row| texts(row))
            .find(|row| row[1].as_deref() == Some("slug"))
            .unwrap();
        assert_eq!(slug[2].as_deref(), Some("1"));

        let schema = run(&mut conn, &dialect.schema_columns_sql("main"), 100, None).await.unwrap();
        assert!(schema.rows.len() >= 4);
    }
}
