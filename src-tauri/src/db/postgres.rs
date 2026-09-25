use std::sync::atomic::AtomicBool;

use sqlx::postgres::{
    PgConnectOptions, PgConnection, PgPoolOptions, PgQueryResult, PgRow, PgSslMode,
};
use sqlx::{Connection, Postgres, Row, TypeInfo, ValueRef};

use super::{
    index_columns_from_definition, quote_double, quote_literal, run_raw, with_timeout, CellValue,
    Conn, Dialect, Opened, Pool, RawOutput, CONNECT_TIMEOUT,
};
use crate::models::ConnectionEntry;

fn ssl_mode(mode: &str) -> PgSslMode {
    match mode {
        "disable" => PgSslMode::Disable,
        "require" => PgSslMode::Require,
        _ => PgSslMode::Prefer,
    }
}

fn options(entry: &ConnectionEntry, password: Option<&str>) -> PgConnectOptions {
    let mut options = PgConnectOptions::new()
        .host(&entry.host)
        .port(entry.port)
        .username(&entry.user)
        .ssl_mode(ssl_mode(&entry.ssl_mode))
        .application_name("Recon");
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
        PgPoolOptions::new()
            .max_connections(4)
            .acquire_timeout(CONNECT_TIMEOUT)
            .connect_with(options.clone()),
    )
    .await?;
    let mut conn = with_timeout(PgConnection::connect_with(&options)).await?;
    let backend_id = sqlx::query_scalar::<_, i32>("SELECT pg_backend_pid()")
        .fetch_one(&mut conn)
        .await
        .ok()
        .map(i64::from);
    Ok(Opened {
        pool: Pool::Postgres(pool),
        conn: Conn::Postgres(conn),
        backend_id,
    })
}

pub async fn test(entry: &ConnectionEntry, password: Option<&str>) -> Result<String, String> {
    let mut conn = with_timeout(PgConnection::connect_with(&options(entry, password))).await?;
    let version = run(&mut conn, PostgresDialect.version_sql(), 1, None).await;
    let _ = conn.close().await;
    Ok(super::first_text(&version?))
}

fn affected(result: &PgQueryResult) -> u64 {
    result.rows_affected()
}

pub async fn run(
    conn: &mut PgConnection,
    sql: &str,
    limit: usize,
    cancel: Option<&AtomicBool>,
) -> Result<RawOutput, String> {
    run_raw::<Postgres>(conn, sql, limit, cancel, cell, affected).await
}

fn text(row: &PgRow, index: usize, wrap: fn(String) -> CellValue) -> CellValue {
    row.try_get_unchecked::<String, _>(index)
        .map(wrap)
        .unwrap_or_else(|_| CellValue::Text(String::new()))
}

pub fn cell(row: &PgRow, index: usize) -> CellValue {
    let type_name = match row.try_get_raw(index) {
        Ok(raw) if raw.is_null() => return CellValue::Null,
        Ok(raw) => raw.type_info().name().to_string(),
        Err(_) => return CellValue::Text(String::new()),
    };
    let typed = match type_name.as_str() {
        "BOOL" => row.try_get_unchecked::<bool, _>(index).ok().map(CellValue::Bool),
        "INT2" => row
            .try_get_unchecked::<i16, _>(index)
            .ok()
            .map(|value| CellValue::Int(value.into())),
        "INT4" => row
            .try_get_unchecked::<i32, _>(index)
            .ok()
            .map(|value| CellValue::Int(value.into())),
        "INT8" => row.try_get_unchecked::<i64, _>(index).ok().map(CellValue::Int),
        "FLOAT4" => row
            .try_get_unchecked::<f32, _>(index)
            .ok()
            .map(|value| CellValue::from_float(value.into())),
        "FLOAT8" => row
            .try_get_unchecked::<f64, _>(index)
            .ok()
            .map(CellValue::from_float),
        "BYTEA" => row
            .try_get_unchecked::<Vec<u8>, _>(index)
            .ok()
            .map(CellValue::Bytes),
        "JSON" | "JSONB" => Some(text(row, index, CellValue::Json)),
        "DATE" | "TIME" | "TIMETZ" | "TIMESTAMP" | "TIMESTAMPTZ" | "INTERVAL" => {
            Some(text(row, index, CellValue::DateTime))
        }
        _ => None,
    };
    typed.unwrap_or_else(|| text(row, index, CellValue::Text))
}

pub struct PostgresDialect;

impl Dialect for PostgresDialect {
    fn version_sql(&self) -> &'static str {
        "SHOW server_version"
    }

    fn namespaces_sql(&self) -> &'static str {
        "SELECT nspname FROM pg_catalog.pg_namespace \
         WHERE nspname NOT LIKE 'pg\\_%' AND nspname <> 'information_schema' ORDER BY nspname"
    }

    fn current_namespace_sql(&self) -> &'static str {
        "SELECT current_schema()"
    }

    fn namespace_label(&self) -> &'static str {
        "Schema"
    }

    fn system_namespaces(&self) -> &'static [&'static str] {
        &[]
    }

    fn tables_sql(&self, namespace: &str) -> String {
        format!(
            "SELECT c.relname, CASE WHEN c.relkind IN ('v', 'm') THEN 'view' ELSE 'table' END \
             FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = {} AND c.relkind IN ('r', 'p', 'v', 'm', 'f') ORDER BY c.relname",
            quote_literal(namespace)
        )
    }

    fn columns_sql(&self, namespace: &str, table: &str) -> String {
        format!(
            "SELECT a.attname, format_type(a.atttypid, a.atttypmod), \
             CASE WHEN a.attnotnull THEN 'NO' ELSE 'YES' END, \
             pg_get_expr(d.adbin, d.adrelid), \
             EXISTS (SELECT 1 FROM pg_catalog.pg_index i \
                     WHERE i.indrelid = a.attrelid AND i.indisprimary AND a.attnum = ANY(i.indkey)), \
             CASE WHEN a.attidentity <> '' THEN 'identity' ELSE '' END \
             FROM pg_catalog.pg_attribute a \
             JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
             WHERE n.nspname = {} AND c.relname = {} AND a.attnum > 0 AND NOT a.attisdropped \
             ORDER BY a.attnum",
            quote_literal(namespace),
            quote_literal(table)
        )
    }

    fn indexes_sql(&self, namespace: &str, table: &str) -> String {
        format!(
            "SELECT i.relname, pg_get_indexdef(ix.indexrelid), ix.indisunique, ix.indisprimary \
             FROM pg_catalog.pg_index ix \
             JOIN pg_catalog.pg_class t ON t.oid = ix.indrelid \
             JOIN pg_catalog.pg_class i ON i.oid = ix.indexrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
             WHERE n.nspname = {} AND t.relname = {} \
             ORDER BY ix.indisprimary DESC, i.relname",
            quote_literal(namespace),
            quote_literal(table)
        )
    }

    fn index_definitions_sql(&self, namespace: &str, table: &str) -> String {
        format!(
            "SELECT i.relname, pg_get_indexdef(ix.indexrelid), ix.indisunique, ix.indisprimary, \
             pg_get_indexdef(ix.indexrelid), '', '', \
             EXISTS (SELECT 1 FROM pg_catalog.pg_constraint con WHERE con.conindid = ix.indexrelid) \
             FROM pg_catalog.pg_index ix \
             JOIN pg_catalog.pg_class t ON t.oid = ix.indrelid \
             JOIN pg_catalog.pg_class i ON i.oid = ix.indexrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
             WHERE n.nspname = {} AND t.relname = {}",
            quote_literal(namespace),
            quote_literal(table)
        )
    }

    fn index_columns(&self, raw: String) -> String {
        index_columns_from_definition(&raw)
    }

    fn schema_columns_sql(&self, namespace: &str) -> String {
        format!(
            "SELECT table_name, column_name FROM information_schema.columns \
             WHERE table_schema = {} ORDER BY table_name, ordinal_position",
            quote_literal(namespace)
        )
    }

    fn quote_ident(&self, ident: &str) -> String {
        quote_double(ident)
    }

    fn use_namespace_sql(&self, namespace: &str) -> Option<String> {
        if namespace == "public" {
            return Some("SET search_path TO public".into());
        }
        Some(format!("SET search_path TO {}, public", quote_double(namespace)))
    }
}
