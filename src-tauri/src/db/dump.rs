use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

use futures_util::TryStreamExt;
use serde::Serialize;
use sqlx::mysql::{MySqlConnection, MySqlRow};
use sqlx::postgres::{PgConnection, PgRow};
use sqlx::sqlite::{SqliteConnection, SqliteRow};
use sqlx::{Database, Executor, Row};

use super::{
    describe_error, hex, mysql, postgres, quote_backtick, quote_double, quote_literal, sqlite, text_at, texts,
    CellValue, Conn, RawOutput,
};

pub const CANCELLED: &str = "Export cancelled.";
const BATCH_ROWS: usize = 500;
const BATCH_BYTES: usize = 512 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct DumpOptions {
    pub structure: bool,
    pub data: bool,
    /// Only applies with `structure`, since dropping without recreating loses the table.
    pub drop_tables: bool,
}

#[derive(Debug, Clone)]
pub struct DumpTable {
    pub name: String,
    pub view: bool,
}

pub struct DumpProgress<'a> {
    pub table: &'a str,
    pub index: usize,
    pub total: usize,
    pub rows: u64,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DumpSummary {
    pub tables: usize,
    pub rows: u64,
    /// Tables left out because Recon can't recreate them yet, with the reason.
    pub skipped: Vec<String>,
}

pub struct Dump<'a> {
    out: &'a mut (dyn Write + Send),
    cancel: &'a AtomicBool,
    progress: &'a mut (dyn FnMut(DumpProgress) + Send),
    index: usize,
    total: usize,
    summary: DumpSummary,
}

impl<'a> Dump<'a> {
    pub fn new(
        out: &'a mut (dyn Write + Send),
        cancel: &'a AtomicBool,
        progress: &'a mut (dyn FnMut(DumpProgress) + Send),
    ) -> Self {
        Dump {
            out,
            cancel,
            progress,
            index: 0,
            total: 0,
            summary: DumpSummary::default(),
        }
    }

    pub fn into_summary(self) -> DumpSummary {
        self.summary
    }

    fn write(&mut self, text: &str) -> Result<(), String> {
        self.out
            .write_all(text.as_bytes())
            .map_err(|err| format!("Could not write the export file: {err}"))
    }

    fn check(&self) -> Result<(), String> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(CANCELLED.into());
        }
        Ok(())
    }

    fn start(&mut self, table: &str) -> Result<(), String> {
        self.check()?;
        self.index += 1;
        self.summary.tables += 1;
        self.report(table, 0);
        self.write(&format!("\n-- {table}\n"))
    }

    fn report(&mut self, table: &str, rows: u64) {
        (self.progress)(DumpProgress {
            table,
            index: self.index,
            total: self.total,
            rows,
        });
    }

    fn skip(&mut self, table: &str, reason: &str) -> Result<(), String> {
        self.summary.skipped.push(format!("{table} ({reason})"));
        self.write(&format!("\n-- Skipped {table}: {reason}\n"))
    }
}

/**
 * Writes `tables` from `namespace` as SQL that recreates them in whichever
 * database it's imported into: names are never qualified with the source.
 * Runs inside one read-only snapshot so the data is consistent.
 */
pub async fn run(
    conn: &mut Conn,
    server: &str,
    namespace: &str,
    tables: &[DumpTable],
    options: DumpOptions,
    dump: &mut Dump<'_>,
) -> Result<(), String> {
    let options = DumpOptions {
        drop_tables: options.drop_tables && options.structure,
        ..options
    };
    dump.total = tables.iter().filter(|table| !table.view || options.structure).count();
    dump.write(&format!("-- Recon SQL export\n-- Server: {server}\n-- Database: {namespace}\n"))?;
    match conn {
        Conn::MySql(conn) => dump_mysql(conn, namespace, tables, options, dump).await,
        Conn::Postgres(conn) => dump_postgres(conn, namespace, tables, options, dump).await,
        Conn::Sqlite(conn) => dump_sqlite(conn, namespace, tables, options, dump).await,
    }
}

fn float_literal(value: f64) -> String {
    let text = value.to_string();
    if !value.is_finite() || text.contains(['.', 'e', 'E']) {
        text
    } else {
        format!("{text}.0")
    }
}

fn mysql_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('\'');
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\0' => out.push_str("\\0"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\x1a' => out.push_str("\\Z"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

pub fn mysql_literal(cell: &CellValue) -> String {
    match cell {
        CellValue::Null => "NULL".into(),
        CellValue::Bool(value) => i64::from(*value).to_string(),
        CellValue::Int(value) => value.to_string(),
        CellValue::Float(value) => float_literal(*value),
        CellValue::Bytes(bytes) if bytes.is_empty() => "''".into(),
        CellValue::Bytes(bytes) => format!("0x{}", hex(bytes)),
        CellValue::Text(text) | CellValue::Json(text) | CellValue::DateTime(text) => mysql_string(text),
    }
}

pub fn sqlite_literal(cell: &CellValue) -> String {
    match cell {
        CellValue::Null => "NULL".into(),
        CellValue::Bool(value) => i64::from(*value).to_string(),
        CellValue::Int(value) => value.to_string(),
        CellValue::Float(value) => float_literal(*value),
        CellValue::Bytes(bytes) => format!("X'{}'", hex(bytes)),
        CellValue::Text(text) | CellValue::Json(text) | CellValue::DateTime(text) => quote_literal(text),
    }
}

fn mysql_value(row: &MySqlRow, index: usize) -> Result<String, String> {
    Ok(mysql_literal(&mysql::cell(row, index)))
}

fn sqlite_value(row: &SqliteRow, index: usize) -> Result<String, String> {
    Ok(sqlite_literal(&sqlite::cell(row, index)))
}

/// Postgres columns are selected as `::text`, and the server casts the quoted text back on insert.
fn postgres_value(row: &PgRow, index: usize) -> Result<String, String> {
    let value = row.try_get::<Option<String>, _>(index).map_err(describe_error)?;
    Ok(value.map_or_else(|| "NULL".into(), |text| quote_literal(&text)))
}

/// Streams `select` into multi-row INSERTs so no table has to fit in memory.
async fn write_rows<DB>(
    dump: &mut Dump<'_>,
    conn: &mut DB::Connection,
    table: &str,
    select: &str,
    insert: &str,
    literal: fn(&DB::Row, usize) -> Result<String, String>,
) -> Result<(), String>
where
    DB: Database,
    for<'e> &'e mut DB::Connection: Executor<'e, Database = DB>,
{
    let mut stream = sqlx::raw_sql(select).fetch(&mut *conn);
    let mut batch = String::new();
    let mut batched = 0;
    let mut rows = 0u64;
    while let Some(row) = stream.try_next().await.map_err(describe_error)? {
        dump.check()?;
        if batched == 0 {
            batch.push_str(insert);
            batch.push_str(" VALUES\n(");
        } else {
            batch.push_str(",\n(");
        }
        for index in 0..row.len() {
            if index > 0 {
                batch.push_str(", ");
            }
            batch.push_str(&literal(&row, index)?);
        }
        batch.push(')');
        batched += 1;
        rows += 1;
        if batched >= BATCH_ROWS || batch.len() >= BATCH_BYTES {
            batch.push_str(";\n");
            dump.write(&batch)?;
            batch.clear();
            batched = 0;
            dump.report(table, rows);
        }
    }
    if batched > 0 {
        batch.push_str(";\n");
        dump.write(&batch)?;
    }
    dump.summary.rows += rows;
    dump.report(table, rows);
    Ok(())
}

fn column_list(columns: &[String], quote: fn(&str) -> String) -> String {
    columns.iter().map(|column| quote(column)).collect::<Vec<_>>().join(", ")
}

fn first_column(output: &RawOutput) -> Vec<String> {
    output
        .text_rows()
        .into_iter()
        .filter_map(|row| row.into_iter().next().flatten())
        .collect()
}

/// Drops the `DEFINER=user@host` clause, since that account rarely exists where the view is imported.
fn strip_definer(sql: &str) -> String {
    let Some(start) = sql.find(" DEFINER=") else {
        return sql.to_string();
    };
    let rest = &sql[start + 1..];
    match rest.find(" SQL SECURITY ").or_else(|| rest.find(" VIEW ")) {
        Some(end) => format!("{}{}", &sql[..start], &rest[end..]),
        None => sql.to_string(),
    }
}

async fn dump_mysql(
    conn: &mut MySqlConnection,
    namespace: &str,
    tables: &[DumpTable],
    options: DumpOptions,
    dump: &mut Dump<'_>,
) -> Result<(), String> {
    for sql in [
        format!("USE {}", quote_backtick(namespace)),
        "SET time_zone = '+00:00'".into(),
        "SET SESSION TRANSACTION ISOLATION LEVEL REPEATABLE READ".into(),
        "START TRANSACTION WITH CONSISTENT SNAPSHOT".into(),
    ] {
        mysql::run(conn, &sql, 0, None).await?;
    }
    dump.write(
        "\nSET NAMES utf8mb4;\n\
         SET @OLD_TIME_ZONE = @@TIME_ZONE, TIME_ZONE = '+00:00';\n\
         SET @OLD_FOREIGN_KEY_CHECKS = @@FOREIGN_KEY_CHECKS, FOREIGN_KEY_CHECKS = 0;\n\
         SET @OLD_UNIQUE_CHECKS = @@UNIQUE_CHECKS, UNIQUE_CHECKS = 0;\n\
         SET @OLD_SQL_MODE = @@SQL_MODE, SQL_MODE = 'NO_AUTO_VALUE_ON_ZERO';\n",
    )?;
    for table in tables.iter().filter(|table| !table.view) {
        dump.start(&table.name)?;
        let quoted = quote_backtick(&table.name);
        if options.drop_tables {
            dump.write(&format!("DROP TABLE IF EXISTS {quoted};\n"))?;
        }
        if options.structure {
            let output = mysql::run(conn, &format!("SHOW CREATE TABLE {quoted}"), 1, None).await?;
            let create = output.text_rows().into_iter().next().map(|row| text_at(&row, 1)).unwrap_or_default();
            dump.write(&format!("{create};\n"))?;
        }
        if options.data {
            let definitions = mysql::column_definitions(
                &mysql::run(conn, &mysql::column_definitions_sql(namespace, &table.name), usize::MAX, None).await?,
            );
            let columns: Vec<String> = definitions
                .into_iter()
                .filter(|column| !column.generated)
                .map(|column| column.name)
                .collect();
            if !columns.is_empty() {
                let list = column_list(&columns, quote_backtick);
                let select = format!("SELECT {list} FROM {quoted}");
                let insert = format!("INSERT INTO {quoted} ({list})");
                write_rows::<sqlx::MySql>(dump, conn, &table.name, &select, &insert, mysql_value).await?;
            }
        }
    }
    if options.structure {
        for view in tables.iter().filter(|table| table.view) {
            dump.start(&view.name)?;
            let quoted = quote_backtick(&view.name);
            if options.drop_tables {
                dump.write(&format!("DROP VIEW IF EXISTS {quoted};\n"))?;
            }
            let output = mysql::run(conn, &format!("SHOW CREATE VIEW {quoted}"), 1, None).await?;
            let create = output.text_rows().into_iter().next().map(|row| text_at(&row, 1)).unwrap_or_default();
            dump.write(&format!("{};\n", strip_definer(&create)))?;
        }
    }
    mysql::run(conn, "COMMIT", 0, None).await?;
    dump.write(
        "\nSET SQL_MODE = @OLD_SQL_MODE;\n\
         SET UNIQUE_CHECKS = @OLD_UNIQUE_CHECKS;\n\
         SET FOREIGN_KEY_CHECKS = @OLD_FOREIGN_KEY_CHECKS;\n\
         SET TIME_ZONE = @OLD_TIME_ZONE;\n",
    )
}

struct PgColumn {
    name: String,
    data_type: String,
    not_null: bool,
    default: Option<String>,
    identity: String,
    generated: String,
}

impl PgColumn {
    fn definition(&self) -> String {
        let mut sql = format!("{} {}", quote_double(&self.name), self.data_type);
        match self.identity.as_str() {
            "a" => sql.push_str(" GENERATED ALWAYS AS IDENTITY"),
            "d" => sql.push_str(" GENERATED BY DEFAULT AS IDENTITY"),
            _ => {}
        }
        match (self.generated.as_str(), &self.default) {
            ("s", Some(expr)) => sql.push_str(&format!(" GENERATED ALWAYS AS ({expr}) STORED")),
            ("v", Some(expr)) => sql.push_str(&format!(" GENERATED ALWAYS AS ({expr}) VIRTUAL")),
            ("", Some(expr)) => sql.push_str(&format!(" DEFAULT {expr}")),
            _ => {}
        }
        if self.not_null && self.identity.is_empty() {
            sql.push_str(" NOT NULL");
        }
        sql
    }
}

struct PgConstraint {
    name: String,
    kind: String,
    definition: String,
}

struct PgSequence {
    column: String,
    name: String,
    qualified: String,
    identity: bool,
}

struct PgRelation {
    kind: String,
    partition: bool,
    columns: Vec<PgColumn>,
    constraints: Vec<PgConstraint>,
    indexes: Vec<String>,
    sequences: Vec<PgSequence>,
    view: Option<String>,
}

/**
 * Everything needed to recreate the namespace's tables, read with a handful
 * of catalog queries rather than several per table, which matters over a
 * slow tunnel.
 */
async fn postgres_catalog(conn: &mut PgConnection, namespace: &str) -> Result<(HashMap<String, PgRelation>, Vec<String>), String> {
    let ns = quote_literal(namespace);
    let relations = postgres::run(
        conn,
        &format!(
            "SELECT c.relname, c.relkind::text, c.relispartition, \
             CASE WHEN c.relkind IN ('v', 'm') THEN pg_get_viewdef(c.oid) END \
             FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = {ns} AND c.relkind IN ('r', 'p', 'v', 'm', 'f') ORDER BY c.oid"
        ),
        usize::MAX,
        None,
    )
    .await?;
    let mut order = Vec::new();
    let mut catalog: HashMap<String, PgRelation> = HashMap::new();
    for row in &relations.rows {
        let text = texts(row);
        let name = text_at(&text, 0);
        order.push(name.clone());
        catalog.insert(
            name,
            PgRelation {
                kind: text_at(&text, 1),
                partition: row.get(2).is_some_and(CellValue::is_truthy),
                columns: Vec::new(),
                constraints: Vec::new(),
                indexes: Vec::new(),
                sequences: Vec::new(),
                view: text.get(3).cloned().flatten(),
            },
        );
    }

    let columns = postgres::run(
        conn,
        &format!(
            "SELECT c.relname, a.attname, format_type(a.atttypid, a.atttypmod), a.attnotnull, \
             pg_get_expr(d.adbin, d.adrelid), a.attidentity::text, a.attgenerated::text \
             FROM pg_catalog.pg_attribute a \
             JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
             WHERE n.nspname = {ns} AND c.relkind IN ('r', 'p') AND a.attnum > 0 AND NOT a.attisdropped \
             ORDER BY c.relname, a.attnum"
        ),
        usize::MAX,
        None,
    )
    .await?;
    for row in &columns.rows {
        let text = texts(row);
        if let Some(relation) = catalog.get_mut(&text_at(&text, 0)) {
            relation.columns.push(PgColumn {
                name: text_at(&text, 1),
                data_type: text_at(&text, 2),
                not_null: row.get(3).is_some_and(CellValue::is_truthy),
                default: text.get(4).cloned().flatten(),
                identity: text_at(&text, 5),
                generated: text_at(&text, 6),
            });
        }
    }

    let constraints = postgres::run(
        conn,
        &format!(
            "SELECT c.relname, con.conname, con.contype::text, pg_get_constraintdef(con.oid) \
             FROM pg_catalog.pg_constraint con \
             JOIN pg_catalog.pg_class c ON c.oid = con.conrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = {ns} AND con.contype IN ('p', 'u', 'c', 'x', 'f') AND con.conislocal \
             ORDER BY c.relname, con.contype = 'p' DESC, con.conname"
        ),
        usize::MAX,
        None,
    )
    .await?;
    for row in constraints.text_rows() {
        if let Some(relation) = catalog.get_mut(&text_at(&row, 0)) {
            relation.constraints.push(PgConstraint {
                name: text_at(&row, 1),
                kind: text_at(&row, 2),
                definition: text_at(&row, 3),
            });
        }
    }

    let indexes = postgres::run(
        conn,
        &format!(
            "SELECT c.relname, pg_get_indexdef(i.indexrelid), format('%I.%I', n.nspname, c.relname) \
             FROM pg_catalog.pg_index i \
             JOIN pg_catalog.pg_class c ON c.oid = i.indrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = {ns} AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint con \
                 WHERE con.conindid = i.indexrelid AND con.conrelid = i.indrelid) \
             ORDER BY c.relname, i.indexrelid"
        ),
        usize::MAX,
        None,
    )
    .await?;
    for row in indexes.text_rows() {
        let table = text_at(&row, 0);
        if let Some(relation) = catalog.get_mut(&table) {
            let definition = text_at(&row, 1).replacen(
                &format!(" {} USING ", text_at(&row, 2)),
                &format!(" {} USING ", quote_double(&table)),
                1,
            );
            relation.indexes.push(definition);
        }
    }

    let sequences = postgres::run(
        conn,
        &format!(
            "SELECT c.relname, a.attname, s.relname, format('%I.%I', sn.nspname, s.relname), a.attidentity <> '' \
             FROM pg_catalog.pg_depend d \
             JOIN pg_catalog.pg_class s ON s.oid = d.objid AND s.relkind = 'S' \
             JOIN pg_catalog.pg_namespace sn ON sn.oid = s.relnamespace \
             JOIN pg_catalog.pg_class c ON c.oid = d.refobjid \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             JOIN pg_catalog.pg_attribute a ON a.attrelid = c.oid AND a.attnum = d.refobjsubid \
             WHERE d.classid = 'pg_catalog.pg_class'::regclass AND d.refclassid = 'pg_catalog.pg_class'::regclass \
             AND d.deptype IN ('a', 'i') AND n.nspname = {ns} ORDER BY c.relname, a.attnum"
        ),
        usize::MAX,
        None,
    )
    .await?;
    for row in &sequences.rows {
        let text = texts(row);
        if let Some(relation) = catalog.get_mut(&text_at(&text, 0)) {
            relation.sequences.push(PgSequence {
                column: text_at(&text, 1),
                name: text_at(&text, 2),
                qualified: text_at(&text, 3),
                identity: row.get(4).is_some_and(CellValue::is_truthy),
            });
        }
    }
    Ok((catalog, order))
}

async fn dump_postgres(
    conn: &mut PgConnection,
    namespace: &str,
    tables: &[DumpTable],
    options: DumpOptions,
    dump: &mut Dump<'_>,
) -> Result<(), String> {
    /*
     * With the search path set to only this schema, pg_get_viewdef and
     * pg_get_expr leave same-schema names unqualified, so the export can be
     * imported into a schema with a different name.
     */
    for sql in [
        format!("SET search_path TO {}", quote_double(namespace)),
        "BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY".into(),
    ] {
        postgres::run(conn, &sql, 0, None).await?;
    }
    let (catalog, order) = postgres_catalog(conn, namespace).await?;
    let wanted: HashSet<&str> = tables.iter().map(|table| table.name.as_str()).collect();
    let selected: Vec<(&String, &PgRelation)> = order
        .iter()
        .filter(|name| wanted.contains(name.as_str()))
        .filter_map(|name| catalog.get(name).map(|relation| (name, relation)))
        .collect();
    let is_view = |relation: &PgRelation| matches!(relation.kind.as_str(), "v" | "m");
    let unsupported = |relation: &PgRelation| match relation.kind.as_str() {
        "p" => Some("partitioned tables aren't supported yet"),
        "f" => Some("foreign tables aren't supported yet"),
        _ if relation.partition => Some("partitions aren't supported yet"),
        _ => None,
    };

    dump.write(
        "\nSET client_encoding = 'UTF8';\n\
         SET standard_conforming_strings = on;\n\
         SET check_function_bodies = false;\n\
         SET client_min_messages = warning;\n",
    )?;

    if options.drop_tables {
        let mut drops = String::from("\n");
        for (name, relation) in &selected {
            for constraint in relation.constraints.iter().filter(|constraint| constraint.kind == "f") {
                drops.push_str(&format!(
                    "ALTER TABLE IF EXISTS {} DROP CONSTRAINT IF EXISTS {};\n",
                    quote_double(name),
                    quote_double(&constraint.name)
                ));
            }
        }
        for (name, relation) in selected.iter().rev().filter(|(_, relation)| is_view(relation)) {
            let kind = if relation.kind == "m" { "MATERIALIZED VIEW" } else { "VIEW" };
            drops.push_str(&format!("DROP {kind} IF EXISTS {};\n", quote_double(name)));
        }
        for (name, relation) in &selected {
            if !is_view(relation) && unsupported(relation).is_none() {
                drops.push_str(&format!("DROP TABLE IF EXISTS {};\n", quote_double(name)));
            }
        }
        dump.write(&drops)?;
    }

    for (name, relation) in selected.iter().filter(|(_, relation)| !is_view(relation)) {
        let quoted = quote_double(name);
        if let Some(reason) = unsupported(relation) {
            dump.skip(name, reason)?;
            continue;
        }
        dump.start(name)?;
        if options.structure {
            let mut sql = String::new();
            for sequence in relation.sequences.iter().filter(|sequence| !sequence.identity) {
                sql.push_str(&format!("CREATE SEQUENCE IF NOT EXISTS {};\n", quote_double(&sequence.name)));
            }
            let mut parts: Vec<String> = relation.columns.iter().map(PgColumn::definition).collect();
            parts.extend(
                relation
                    .constraints
                    .iter()
                    .filter(|constraint| constraint.kind != "f")
                    .map(|constraint| format!("CONSTRAINT {} {}", quote_double(&constraint.name), constraint.definition)),
            );
            sql.push_str(&format!("CREATE TABLE {quoted} (\n    {}\n);\n", parts.join(",\n    ")));
            for sequence in relation.sequences.iter().filter(|sequence| !sequence.identity) {
                sql.push_str(&format!(
                    "ALTER SEQUENCE {} OWNED BY {quoted}.{};\n",
                    quote_double(&sequence.name),
                    quote_double(&sequence.column)
                ));
            }
            dump.write(&sql)?;
        }
        if options.data {
            let columns: Vec<&PgColumn> = relation.columns.iter().filter(|column| column.generated.is_empty()).collect();
            if !columns.is_empty() {
                let names: Vec<String> = columns.iter().map(|column| column.name.clone()).collect();
                let list = column_list(&names, quote_double);
                let select = format!(
                    "SELECT {} FROM ONLY {quoted}",
                    names.iter().map(|name| format!("{}::text", quote_double(name))).collect::<Vec<_>>().join(", ")
                );
                let overriding = if columns.iter().any(|column| column.identity == "a") {
                    " OVERRIDING SYSTEM VALUE"
                } else {
                    ""
                };
                let insert = format!("INSERT INTO {quoted} ({list}){overriding}");
                write_rows::<sqlx::Postgres>(dump, conn, name, &select, &insert, postgres_value).await?;
            }
            for sequence in &relation.sequences {
                let output = postgres::run(conn, &format!("SELECT last_value, is_called FROM {}", sequence.qualified), 1, None).await?;
                if let Some(row) = output.rows.first() {
                    let last = row.first().and_then(CellValue::to_text).unwrap_or_else(|| "1".into());
                    let called = row.get(1).is_some_and(CellValue::is_truthy);
                    dump.write(&format!(
                        "SELECT pg_catalog.setval(pg_catalog.pg_get_serial_sequence({}, {}), {last}, {called});\n",
                        quote_literal(&quoted),
                        quote_literal(&sequence.column)
                    ))?;
                }
            }
        }
        if options.structure && !relation.indexes.is_empty() {
            dump.write(&relation.indexes.iter().map(|index| format!("{index};\n")).collect::<String>())?;
        }
    }

    if options.structure {
        for (name, relation) in selected.iter().filter(|(_, relation)| is_view(relation)) {
            dump.start(name)?;
            let definition = relation.view.as_deref().unwrap_or_default().trim().trim_end_matches(';');
            let sql = if relation.kind == "m" {
                let mut sql = format!("CREATE MATERIALIZED VIEW {} AS\n{definition}\nWITH DATA;\n", quote_double(name));
                for index in &relation.indexes {
                    sql.push_str(&format!("{index};\n"));
                }
                sql
            } else {
                format!("CREATE VIEW {} AS\n{definition};\n", quote_double(name))
            };
            dump.write(&sql)?;
        }
        let mut keys = String::new();
        for (name, relation) in selected.iter().filter(|(_, relation)| unsupported(relation).is_none()) {
            for constraint in relation.constraints.iter().filter(|constraint| constraint.kind == "f") {
                keys.push_str(&format!(
                    "ALTER TABLE {} ADD CONSTRAINT {} {};\n",
                    quote_double(name),
                    quote_double(&constraint.name),
                    constraint.definition
                ));
            }
        }
        if !keys.is_empty() {
            dump.write(&format!("\n-- Foreign keys\n{keys}"))?;
        }
    }
    postgres::run(conn, "COMMIT", 0, None).await?;
    Ok(())
}

async fn dump_sqlite(
    conn: &mut SqliteConnection,
    namespace: &str,
    tables: &[DumpTable],
    options: DumpOptions,
    dump: &mut Dump<'_>,
) -> Result<(), String> {
    let schema = quote_double(namespace);
    sqlite::run(conn, "BEGIN", 0, None).await?;
    let master = sqlite::run(
        conn,
        &format!(
            "SELECT type, name, tbl_name, sql FROM {schema}.sqlite_master \
             WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY rowid"
        ),
        usize::MAX,
        None,
    )
    .await?
    .text_rows();
    let entries: Vec<(String, String, String, String)> = master
        .iter()
        .map(|row| (text_at(row, 0), text_at(row, 1), text_at(row, 2), text_at(row, 3)))
        .collect();
    let virtual_tables: Vec<&str> = entries
        .iter()
        .filter(|(kind, _, _, sql)| kind == "table" && sql.to_ascii_uppercase().starts_with("CREATE VIRTUAL TABLE"))
        .map(|(_, name, _, _)| name.as_str())
        .collect();
    let shadow = |name: &str| virtual_tables.iter().any(|parent| name.starts_with(&format!("{parent}_")));
    let wanted: HashSet<&str> = tables.iter().map(|table| table.name.as_str()).collect();
    let find = |name: &str, kind: &str| entries.iter().find(|entry| entry.0 == kind && entry.1 == name);

    dump.write("\nPRAGMA foreign_keys = OFF;\nBEGIN TRANSACTION;\n")?;
    for table in tables.iter().filter(|table| !table.view) {
        if shadow(&table.name) {
            continue;
        }
        let Some((_, name, _, create)) = find(&table.name, "table") else {
            continue;
        };
        dump.start(name)?;
        let quoted = quote_double(name);
        if options.drop_tables {
            dump.write(&format!("DROP TABLE IF EXISTS {quoted};\n"))?;
        }
        if options.structure {
            dump.write(&format!("{create};\n"))?;
        }
        if options.data {
            let columns = first_column(
                &sqlite::run(
                    conn,
                    &format!(
                        "SELECT name FROM pragma_table_xinfo({}, {}) WHERE hidden = 0 ORDER BY cid",
                        quote_literal(name),
                        quote_literal(namespace)
                    ),
                    usize::MAX,
                    None,
                )
                .await?,
            );
            if !columns.is_empty() {
                let list = column_list(&columns, quote_double);
                let select = format!("SELECT {list} FROM {schema}.{quoted}");
                let insert = format!("INSERT INTO {quoted} ({list})");
                write_rows::<sqlx::Sqlite>(dump, conn, name, &select, &insert, sqlite_value).await?;
            }
        }
        if options.structure {
            for (_, _, _, sql) in entries.iter().filter(|entry| entry.0 == "index" && entry.2 == *name) {
                dump.write(&format!("{sql};\n"))?;
            }
        }
    }
    if options.structure {
        for view in tables.iter().filter(|table| table.view) {
            let Some((_, name, _, create)) = find(&view.name, "view") else {
                continue;
            };
            dump.start(name)?;
            if options.drop_tables {
                dump.write(&format!("DROP VIEW IF EXISTS {};\n", quote_double(name)))?;
            }
            dump.write(&format!("{create};\n"))?;
        }
        let triggers: String = entries
            .iter()
            .filter(|entry| entry.0 == "trigger" && wanted.contains(entry.2.as_str()))
            .map(|entry| format!("{};\n", entry.3))
            .collect();
        if !triggers.is_empty() {
            dump.write(&format!("\n-- Triggers\n{triggers}"))?;
        }
    }
    sqlite::run(conn, "COMMIT", 0, None).await?;
    dump.write("COMMIT;\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sql_split::Splitter;
    use crate::models::Driver;
    use sqlx::Connection;

    #[test]
    fn formats_literals_for_each_driver() {
        assert_eq!(mysql_literal(&CellValue::Text("it's a\\b\n".into())), "'it\\'s a\\\\b\\n'");
        assert_eq!(mysql_literal(&CellValue::Bytes(vec![0xca, 0xfe])), "0xcafe");
        assert_eq!(mysql_literal(&CellValue::Bytes(Vec::new())), "''");
        assert_eq!(mysql_literal(&CellValue::Float(2.0)), "2.0");
        assert_eq!(sqlite_literal(&CellValue::Text("o'b".into())), "'o''b'");
        assert_eq!(sqlite_literal(&CellValue::Bytes(vec![0x01])), "X'01'");
        assert_eq!(sqlite_literal(&CellValue::Float(0.25)), "0.25");
    }

    #[test]
    fn strips_view_definers() {
        assert_eq!(
            strip_definer("CREATE ALGORITHM=UNDEFINED DEFINER=`root`@`%` SQL SECURITY DEFINER VIEW `v` AS select 1"),
            "CREATE ALGORITHM=UNDEFINED SQL SECURITY DEFINER VIEW `v` AS select 1"
        );
        assert_eq!(strip_definer("CREATE VIEW v AS select 1"), "CREATE VIEW v AS select 1");
    }

    async fn export_sqlite(conn: &mut SqliteConnection, tables: &[DumpTable], options: DumpOptions) -> (String, DumpSummary) {
        let mut out: Vec<u8> = Vec::new();
        let cancel = AtomicBool::new(false);
        let mut progress = |_: DumpProgress| {};
        let mut dump = Dump::new(&mut out, &cancel, &mut progress);
        let mut wrapped = Conn::Sqlite(std::mem::replace(conn, SqliteConnection::connect("sqlite::memory:").await.unwrap()));
        run(&mut wrapped, "SQLite 3", "main", tables, options, &mut dump).await.unwrap();
        let summary = dump.into_summary();
        if let Conn::Sqlite(inner) = wrapped {
            *conn = inner;
        }
        (String::from_utf8(out).unwrap(), summary)
    }

    #[tokio::test]
    async fn round_trips_a_sqlite_database() {
        let mut source = SqliteConnection::connect("sqlite::memory:").await.unwrap();
        for sql in [
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, score REAL, avatar BLOB, \
             doubled INT GENERATED ALWAYS AS (id * 2) VIRTUAL)",
            "CREATE INDEX users_name ON users (name)",
            "CREATE TABLE posts (id INTEGER PRIMARY KEY, user_id INT REFERENCES users(id), body TEXT)",
            "CREATE VIEW names AS SELECT name FROM users",
            "CREATE TRIGGER touch AFTER INSERT ON posts BEGIN UPDATE users SET score = score + 1 WHERE id = NEW.user_id; END",
            "INSERT INTO users (id, name, score, avatar) VALUES (1, 'o''brien; x', 2.0, x'cafe'), (2, 'line\nbreak', NULL, NULL)",
            "INSERT INTO posts VALUES (1, 1, '-- not a comment')",
        ] {
            sqlite::run(&mut source, sql, 0, None).await.unwrap();
        }
        let tables = [
            DumpTable { name: "names".into(), view: true },
            DumpTable { name: "posts".into(), view: false },
            DumpTable { name: "users".into(), view: false },
        ];
        let options = DumpOptions { structure: true, data: true, drop_tables: true };
        let (sql, summary) = export_sqlite(&mut source, &tables, options).await;
        assert_eq!(summary.tables, 3);
        assert_eq!(summary.rows, 3);
        assert!(!sql.contains("doubled)"), "{sql}");

        let mut target = SqliteConnection::connect("sqlite::memory:").await.unwrap();
        let statements = split_all(Driver::Sqlite, sql.as_bytes());
        for statement in &statements {
            sqlx::raw_sql(&statement.sql).execute(&mut target).await.unwrap_or_else(|err| panic!("{err}: {}", statement.sql));
        }
        let users = sqlite::run(&mut target, "SELECT id, name, score, avatar, doubled FROM users ORDER BY id", 10, None).await.unwrap();
        assert_eq!(
            users.rows[0],
            vec![
                CellValue::Int(1),
                CellValue::Text("o'brien; x".into()),
                CellValue::Float(3.0),
                CellValue::Bytes(vec![0xca, 0xfe]),
                CellValue::Int(2),
            ]
        );
        assert_eq!(users.rows[1][1], CellValue::Text("line\nbreak".into()));
        let names = sqlite::run(&mut target, "SELECT count(*) FROM names", 1, None).await.unwrap();
        assert_eq!(names.rows[0][0], CellValue::Int(2));
        sqlite::run(&mut target, "INSERT INTO posts VALUES (2, 2, 'x')", 0, None).await.unwrap();
        let scored = sqlite::run(&mut target, "SELECT score FROM users WHERE id = 2", 1, None).await.unwrap();
        assert_eq!(scored.rows[0][0], CellValue::Null);
        let indexes = sqlite::run(&mut target, "SELECT name FROM sqlite_master WHERE type = 'index'", 10, None).await.unwrap();
        assert_eq!(indexes.rows.len(), 1);

        let (again, _) = export_sqlite(&mut target, &tables, options).await;
        assert!(again.contains("INSERT INTO \"posts\""));
    }

    fn split_all(driver: Driver, sql: &[u8]) -> Vec<crate::db::sql_split::Statement> {
        let mut splitter = Splitter::new(driver);
        let mut statements = Vec::new();
        for line in sql.split_inclusive(|byte| *byte == b'\n') {
            splitter.push_bytes(line, &mut statements).unwrap();
        }
        splitter.finish(&mut statements);
        statements
    }

    /**
     * Exports a scratch database from a real server, imports it into another,
     * and does the same with `mysqldump` output when RECON_TEST_MYSQLDUMP
     * points at it: `RECON_TEST_MYSQL_USER=root cargo test -- --ignored`.
     */
    #[tokio::test]
    #[ignore]
    async fn live_mysql_round_trip() {
        let Ok(user) = std::env::var("RECON_TEST_MYSQL_USER") else {
            return;
        };
        let entry: crate::models::ConnectionEntry = serde_json::from_value(serde_json::json!({
            "id": "live", "name": "live", "driver": "mysql", "host": "127.0.0.1", "port": 3306,
            "user": user, "database": "", "filePath": "", "sslMode": "prefer", "headerColor": "",
            "savePassword": false,
        }))
        .unwrap();
        let password = std::env::var("RECON_TEST_MYSQL_PASSWORD").ok();
        let opened = mysql::open(&entry, password.as_deref()).await.unwrap();
        let mut conn = opened.conn;
        for sql in [
            "DROP DATABASE IF EXISTS recon_dump_src",
            "DROP DATABASE IF EXISTS recon_dump_dst",
            "CREATE DATABASE recon_dump_src",
            "CREATE DATABASE recon_dump_dst",
            "USE recon_dump_src",
            "CREATE TABLE users (id INT AUTO_INCREMENT PRIMARY KEY, name VARCHAR(40) NOT NULL, \
             big BIGINT UNSIGNED, price DECIMAL(8,2), ratio DOUBLE, doc JSON, at DATETIME(3), \
             raw VARBINARY(8), bits BIT(4), note TEXT, doubled DOUBLE AS (ratio * 2) VIRTUAL, KEY by_name (name))",
            "CREATE TABLE posts (id INT PRIMARY KEY, user_id INT, body LONGTEXT, \
             FOREIGN KEY (user_id) REFERENCES users (id))",
            "CREATE VIEW names AS SELECT name FROM users",
            "INSERT INTO users (id, name, big, price, ratio, doc, at, raw, bits, note) VALUES \
             (0, 'zero', NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL), \
             (5, 'it''s a \\\\ \\n; test', 18446744073709551615, 12.50, 0.1, '{\"a\": [1, \"x\"]}', \
              '2026-01-02 03:04:05.678', 0x00CAFE, b'1010', '日本語 🎉')",
            "INSERT INTO posts VALUES (1, 5, '-- not a comment; /* nor this */')",
        ] {
            conn.execute(sql).await.unwrap_or_else(|err| panic!("{err}: {sql}"));
        }

        let tables = [
            DumpTable { name: "names".into(), view: true },
            DumpTable { name: "posts".into(), view: false },
            DumpTable { name: "users".into(), view: false },
        ];
        let mut out: Vec<u8> = Vec::new();
        let cancel = AtomicBool::new(false);
        let mut progress = |_: DumpProgress| {};
        let mut dump = Dump::new(&mut out, &cancel, &mut progress);
        let options = DumpOptions { structure: true, data: true, drop_tables: true };
        run(&mut conn, "MySQL", "recon_dump_src", &tables, options, &mut dump).await.unwrap();
        let sql = String::from_utf8(out).unwrap();
        assert!(!sql.contains("recon_dump_src`."), "{sql}");
        assert!(!sql.contains("DEFINER="), "{sql}");

        let compare = "SELECT id, name, big, price, ratio, doc, at, HEX(raw), bits + 0, note, doubled FROM users ORDER BY id";
        let expected = conn.run(compare, 10, None).await.unwrap().text_rows();
        async fn import(conn: &mut Conn, sql: Vec<u8>) {
            conn.execute("USE recon_dump_dst").await.unwrap();
            for statement in split_all(Driver::Mysql, &sql) {
                conn.execute(&statement.sql)
                    .await
                    .unwrap_or_else(|err| panic!("line {}: {err}: {}", statement.line, statement.sql));
            }
        }
        import(&mut conn, sql.clone().into_bytes()).await;
        assert_eq!(conn.run(compare, 10, None).await.unwrap().text_rows(), expected);
        let names = conn.run("SELECT COUNT(*) FROM names", 1, None).await.unwrap();
        assert_eq!(names.rows[0][0].as_i64(), Some(2));
        let body = conn.run("SELECT body FROM posts", 1, None).await.unwrap();
        assert_eq!(body.text_rows()[0][0].as_deref(), Some("-- not a comment; /* nor this */"));

        import(&mut conn, sql.into_bytes()).await;
        assert_eq!(conn.run(compare, 10, None).await.unwrap().text_rows(), expected);

        if let Ok(mysqldump) = std::env::var("RECON_TEST_MYSQLDUMP") {
            let output = std::process::Command::new(mysqldump)
                .args(["-h", "127.0.0.1", "-u", &entry.user, "--routines", "--triggers", "recon_dump_src"])
                .output()
                .unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            conn.execute("DROP DATABASE recon_dump_dst").await.unwrap();
            conn.execute("CREATE DATABASE recon_dump_dst").await.unwrap();
            import(&mut conn, output.stdout).await;
            assert_eq!(conn.run(compare, 10, None).await.unwrap().text_rows(), expected);
        }

        conn.execute("DROP DATABASE recon_dump_src").await.unwrap();
        conn.execute("DROP DATABASE recon_dump_dst").await.unwrap();
        conn.close().await;
        opened.pool.close().await;
    }

    #[tokio::test]
    async fn exports_only_the_chosen_parts() {
        let mut conn = SqliteConnection::connect("sqlite::memory:").await.unwrap();
        sqlite::run(&mut conn, "CREATE TABLE t (a INT)", 0, None).await.unwrap();
        sqlite::run(&mut conn, "INSERT INTO t VALUES (1)", 0, None).await.unwrap();
        let tables = [DumpTable { name: "t".into(), view: false }];
        let (data_only, _) = export_sqlite(&mut conn, &tables, DumpOptions { structure: false, data: true, drop_tables: true }).await;
        assert!(!data_only.contains("CREATE TABLE") && !data_only.contains("DROP TABLE"));
        assert!(data_only.contains("INSERT INTO \"t\" (\"a\") VALUES\n(1);"));
        let (structure_only, _) = export_sqlite(&mut conn, &tables, DumpOptions { structure: true, data: false, drop_tables: false }).await;
        assert!(structure_only.contains("CREATE TABLE t (a INT);") && !structure_only.contains("INSERT"));
    }
}
