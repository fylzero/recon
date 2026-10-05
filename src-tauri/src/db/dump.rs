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
    /**
     * Every routine in the namespace, rather than only the Postgres functions
     * the chosen tables use. Meant for whole-namespace exports, since a
     * routine can reference tables that a partial export leaves out.
     */
    pub routines: bool,
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

pub fn mysql_string(text: &str) -> String {
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

/// Drops the `DEFINER=user@host` clause, since that account rarely exists where the object is imported.
fn strip_definer(sql: &str) -> String {
    let Some(start) = sql.find(" DEFINER=") else {
        return sql.to_string();
    };
    let rest = &sql[start + 1..];
    let end = [" SQL SECURITY ", " VIEW ", " TRIGGER ", " PROCEDURE ", " FUNCTION ", " EVENT "]
        .iter()
        .filter_map(|keyword| rest.find(keyword))
        .min();
    match end {
        Some(end) => format!("{}{}", &sql[..start], &rest[end..]),
        None => sql.to_string(),
    }
}

/**
 * Orders `items` so each comes after the ones it `depends` on, otherwise
 * keeping their order. A cycle is broken by taking the first remaining item.
 */
fn dependency_order<T>(items: Vec<T>, depends: impl Fn(&T, &T) -> bool) -> Vec<T> {
    let mut remaining = items;
    let mut ordered = Vec::with_capacity(remaining.len());
    while !remaining.is_empty() {
        let next = (0..remaining.len())
            .find(|&i| {
                !remaining
                    .iter()
                    .enumerate()
                    .any(|(j, other)| j != i && depends(&remaining[i], other))
            })
            .unwrap_or(0);
        ordered.push(remaining.remove(next));
    }
    ordered
}

/// A trigger or routine, wrapped so its `;`-separated body imports as one statement.
fn mysql_compound(sql_mode: &str, create: &str) -> String {
    format!(
        "SET SQL_MODE = {};\nDELIMITER ;;\n{};;\nDELIMITER ;\n",
        mysql_string(sql_mode),
        strip_definer(create)
    )
}

/// The sql_mode and statement from `SHOW CREATE TRIGGER`, `PROCEDURE`, or `FUNCTION`.
async fn mysql_show_create(conn: &mut MySqlConnection, kind: &str, name: &str) -> Option<(String, String)> {
    let output = mysql::run(conn, &format!("SHOW CREATE {kind} {}", quote_backtick(name)), 1, None)
        .await
        .ok()?;
    let row = output.text_rows().into_iter().next()?;
    let create = row.get(2).cloned().flatten()?;
    Some((text_at(&row, 1), create))
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
        let schema = mysql_string(namespace);
        if options.routines {
            let routines = mysql::run(
                conn,
                &format!(
                    "SELECT ROUTINE_TYPE, ROUTINE_NAME FROM information_schema.ROUTINES \
                     WHERE ROUTINE_SCHEMA = {schema} ORDER BY ROUTINE_TYPE, ROUTINE_NAME"
                ),
                usize::MAX,
                None,
            )
            .await?
            .text_rows();
            if !routines.is_empty() {
                dump.write("\n-- Routines\n")?;
            }
            for row in &routines {
                dump.check()?;
                let (kind, name) = (text_at(row, 0), text_at(row, 1));
                let Some((sql_mode, create)) = mysql_show_create(conn, &kind, &name).await else {
                    dump.skip(&name, "no permission to read its definition")?;
                    continue;
                };
                if options.drop_tables {
                    dump.write(&format!("DROP {kind} IF EXISTS {};\n", quote_backtick(&name)))?;
                }
                dump.write(&mysql_compound(&sql_mode, &create))?;
            }
            if !routines.is_empty() {
                dump.write("SET SQL_MODE = 'NO_AUTO_VALUE_ON_ZERO';\n")?;
            }
        }

        let mut views = Vec::new();
        for view in tables.iter().filter(|table| table.view) {
            dump.check()?;
            let output = mysql::run(conn, &format!("SHOW CREATE VIEW {}", quote_backtick(&view.name)), 1, None).await?;
            let create = output.text_rows().into_iter().next().map(|row| text_at(&row, 1)).unwrap_or_default();
            views.push((view.name.clone(), strip_definer(&create)));
        }
        /*
         * MySQL writes every identifier in a view's definition in backticks,
         * so a mention of another view's quoted name is a dependency on it.
         */
        let views = dependency_order(views, |(name, create), (other, _)| {
            name != other && create.contains(&quote_backtick(other))
        });
        for (name, create) in &views {
            dump.start(name)?;
            if options.drop_tables {
                dump.write(&format!("DROP VIEW IF EXISTS {};\n", quote_backtick(name)))?;
            }
            dump.write(&format!("{create};\n"))?;
        }

        let chosen: HashSet<&str> = tables.iter().filter(|table| !table.view).map(|table| table.name.as_str()).collect();
        let triggers = mysql::run(
            conn,
            &format!(
                "SELECT TRIGGER_NAME, EVENT_OBJECT_TABLE FROM information_schema.TRIGGERS \
                 WHERE TRIGGER_SCHEMA = {schema} \
                 ORDER BY EVENT_OBJECT_TABLE, ACTION_TIMING, EVENT_MANIPULATION, ACTION_ORDER"
            ),
            usize::MAX,
            None,
        )
        .await?
        .text_rows();
        let triggers: Vec<String> = triggers
            .iter()
            .filter(|row| chosen.contains(text_at(row, 1).as_str()))
            .map(|row| text_at(row, 0))
            .collect();
        if !triggers.is_empty() {
            dump.write("\n-- Triggers\n")?;
        }
        for name in &triggers {
            dump.check()?;
            let Some((sql_mode, create)) = mysql_show_create(conn, "TRIGGER", name).await else {
                dump.skip(name, "no permission to read its definition")?;
                continue;
            };
            dump.write(&mysql_compound(&sql_mode, &create))?;
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
    /// Everything after the name in `CREATE SEQUENCE`, starting with a space.
    options: String,
}

struct PgTrigger {
    name: String,
    definition: String,
    enabled: String,
}

struct PgRelation {
    kind: String,
    partition: bool,
    columns: Vec<PgColumn>,
    constraints: Vec<PgConstraint>,
    indexes: Vec<String>,
    /// Sequences the table owns, through serial or identity columns.
    sequences: Vec<PgSequence>,
    /// Sequences column defaults draw from, whether or not the table owns them.
    borrowed: Vec<PgSequence>,
    view: Option<String>,
    /// Other views and materialized views in the namespace this view reads from.
    reads: Vec<String>,
    comment: Option<String>,
    column_comments: Vec<(String, String)>,
    triggers: Vec<PgTrigger>,
}

struct PgFunction {
    oid: String,
    /// The name and argument types, as `DROP ROUTINE` takes them.
    signature: String,
    definition: String,
    /// Takes or returns a table's row type, so it can only be created after the tables.
    after_tables: bool,
}

/// A function in the namespace used by `relation`, by a type when both are empty, or by another `function`.
struct PgFunctionUse {
    function: String,
    relation: Option<String>,
    caller: Option<String>,
}

/// Namespace-level objects the tables can depend on, only read when exporting structure.
struct PgObjects {
    extensions: Vec<String>,
    types: Vec<String>,
    functions: Vec<PgFunction>,
    uses: Vec<PgFunctionUse>,
}

const PG_SEQUENCE_OPTIONS: &str = "format(' AS %s INCREMENT BY %s MINVALUE %s MAXVALUE %s START WITH %s CACHE %s%s', \
     format_type(q.seqtypid, NULL), q.seqincrement, q.seqmin, q.seqmax, q.seqstart, q.seqcache, \
     CASE WHEN q.seqcycle THEN ' CYCLE' ELSE '' END)";

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
                borrowed: Vec::new(),
                view: text.get(3).cloned().flatten(),
                reads: Vec::new(),
                comment: None,
                column_comments: Vec::new(),
                triggers: Vec::new(),
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
            "SELECT c.relname, a.attname, s.relname, format('%I.%I', sn.nspname, s.relname), a.attidentity <> '', \
             {PG_SEQUENCE_OPTIONS} \
             FROM pg_catalog.pg_depend d \
             JOIN pg_catalog.pg_class s ON s.oid = d.objid AND s.relkind = 'S' \
             JOIN pg_catalog.pg_namespace sn ON sn.oid = s.relnamespace \
             JOIN pg_catalog.pg_sequence q ON q.seqrelid = s.oid \
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
                options: text_at(&text, 5),
            });
        }
    }

    let borrowed = postgres::run(
        conn,
        &format!(
            "SELECT DISTINCT c.relname, s.relname, format('%I.%I', sn.nspname, s.relname), {PG_SEQUENCE_OPTIONS} \
             FROM pg_catalog.pg_depend d \
             JOIN pg_catalog.pg_attrdef ad ON ad.oid = d.objid \
             JOIN pg_catalog.pg_class c ON c.oid = ad.adrelid \
             JOIN pg_catalog.pg_class s ON s.oid = d.refobjid AND s.relkind = 'S' \
             JOIN pg_catalog.pg_namespace sn ON sn.oid = s.relnamespace \
             JOIN pg_catalog.pg_sequence q ON q.seqrelid = s.oid \
             WHERE d.classid = 'pg_catalog.pg_attrdef'::regclass AND d.refclassid = 'pg_catalog.pg_class'::regclass \
             AND c.relnamespace = sn.oid AND sn.nspname = {ns} ORDER BY c.relname, s.relname"
        ),
        usize::MAX,
        None,
    )
    .await?;
    for row in borrowed.text_rows() {
        if let Some(relation) = catalog.get_mut(&text_at(&row, 0)) {
            relation.borrowed.push(PgSequence {
                column: String::new(),
                name: text_at(&row, 1),
                qualified: text_at(&row, 2),
                identity: false,
                options: text_at(&row, 3),
            });
        }
    }

    let reads = postgres::run(
        conn,
        &format!(
            "SELECT DISTINCT v.relname, r.relname FROM pg_catalog.pg_depend d \
             JOIN pg_catalog.pg_rewrite w ON w.oid = d.objid \
             JOIN pg_catalog.pg_class v ON v.oid = w.ev_class \
             JOIN pg_catalog.pg_class r ON r.oid = d.refobjid \
             JOIN pg_catalog.pg_namespace n ON n.oid = v.relnamespace \
             WHERE d.classid = 'pg_catalog.pg_rewrite'::regclass AND d.refclassid = 'pg_catalog.pg_class'::regclass \
             AND n.nspname = {ns} AND r.relnamespace = v.relnamespace AND r.oid <> v.oid AND r.relkind IN ('v', 'm')"
        ),
        usize::MAX,
        None,
    )
    .await?;
    for row in reads.text_rows() {
        if let Some(relation) = catalog.get_mut(&text_at(&row, 0)) {
            relation.reads.push(text_at(&row, 1));
        }
    }

    let comments = postgres::run(
        conn,
        &format!(
            "SELECT c.relname, a.attname, d.description FROM pg_catalog.pg_description d \
             JOIN pg_catalog.pg_class c ON c.oid = d.objoid \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             LEFT JOIN pg_catalog.pg_attribute a ON a.attrelid = c.oid AND a.attnum = d.objsubid AND d.objsubid > 0 \
             WHERE d.classoid = 'pg_catalog.pg_class'::regclass AND n.nspname = {ns} \
             AND (d.objsubid = 0 OR a.attnum IS NOT NULL) ORDER BY c.relname, d.objsubid"
        ),
        usize::MAX,
        None,
    )
    .await?;
    for row in comments.text_rows() {
        if let Some(relation) = catalog.get_mut(&text_at(&row, 0)) {
            let comment = text_at(&row, 2);
            match row.get(1).cloned().flatten() {
                Some(column) => relation.column_comments.push((column, comment)),
                None => relation.comment = Some(comment),
            }
        }
    }

    /*
     * The pretty form of pg_get_triggerdef leaves the table and function
     * unqualified when the search path makes them visible, which the plain
     * form doesn't.
     */
    let triggers = postgres::run(
        conn,
        &format!(
            "SELECT c.relname, quote_ident(t.tgname), pg_get_triggerdef(t.oid, true), t.tgenabled::text \
             FROM pg_catalog.pg_trigger t \
             JOIN pg_catalog.pg_class c ON c.oid = t.tgrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = {ns} AND NOT t.tgisinternal ORDER BY c.relname, t.tgname"
        ),
        usize::MAX,
        None,
    )
    .await?;
    for row in triggers.text_rows() {
        if let Some(relation) = catalog.get_mut(&text_at(&row, 0)) {
            relation.triggers.push(PgTrigger {
                name: text_at(&row, 1),
                definition: text_at(&row, 2),
                enabled: text_at(&row, 3),
            });
        }
    }
    Ok((catalog, order))
}

/**
 * The extensions, types, and functions the namespace's tables can depend on.
 * Objects that belong to an extension are left to `CREATE EXTENSION`.
 */
async fn postgres_objects(conn: &mut PgConnection, namespace: &str) -> Result<PgObjects, String> {
    let ns = quote_literal(namespace);
    let ns_oid = format!("(SELECT oid FROM pg_catalog.pg_namespace WHERE nspname = {ns})");
    /*
     * Leaves out extension members and objects Postgres creates alongside
     * another, like a range type's constructor functions.
     */
    let created_directly = |class: &str, oid: &str| {
        format!(
            "NOT EXISTS (SELECT 1 FROM pg_catalog.pg_depend x WHERE x.classid = 'pg_catalog.{class}'::regclass \
             AND x.objid = {oid} AND x.deptype IN ('e', 'i'))"
        )
    };

    /*
     * An extension is needed when it's installed in the namespace or when
     * something in the namespace, like a column type or a default, depends
     * on one of its members.
     */
    let extensions = postgres::run(
        conn,
        &format!(
            "WITH local_objects (classid, objid) AS ( \
                 SELECT 'pg_catalog.pg_class'::regclass, c.oid FROM pg_catalog.pg_class c WHERE c.relnamespace = {ns_oid} \
                 UNION ALL SELECT 'pg_catalog.pg_attrdef'::regclass, ad.oid FROM pg_catalog.pg_attrdef ad \
                     JOIN pg_catalog.pg_class c ON c.oid = ad.adrelid WHERE c.relnamespace = {ns_oid} \
                 UNION ALL SELECT 'pg_catalog.pg_constraint'::regclass, con.oid FROM pg_catalog.pg_constraint con \
                     WHERE con.connamespace = {ns_oid} \
                 UNION ALL SELECT 'pg_catalog.pg_rewrite'::regclass, w.oid FROM pg_catalog.pg_rewrite w \
                     JOIN pg_catalog.pg_class c ON c.oid = w.ev_class WHERE c.relnamespace = {ns_oid} \
                 UNION ALL SELECT 'pg_catalog.pg_trigger'::regclass, t.oid FROM pg_catalog.pg_trigger t \
                     JOIN pg_catalog.pg_class c ON c.oid = t.tgrelid WHERE c.relnamespace = {ns_oid} \
                 UNION ALL SELECT 'pg_catalog.pg_proc'::regclass, p.oid FROM pg_catalog.pg_proc p WHERE p.pronamespace = {ns_oid} \
                 UNION ALL SELECT 'pg_catalog.pg_type'::regclass, t.oid FROM pg_catalog.pg_type t WHERE t.typnamespace = {ns_oid} \
             ) \
             SELECT quote_ident(e.extname), quote_ident(en.nspname), en.nspname = {ns} \
             FROM pg_catalog.pg_extension e JOIN pg_catalog.pg_namespace en ON en.oid = e.extnamespace \
             WHERE e.extname <> 'plpgsql' AND (e.extnamespace = {ns_oid} OR EXISTS ( \
                 SELECT 1 FROM pg_catalog.pg_depend m \
                 JOIN pg_catalog.pg_depend d ON d.refclassid = m.classid AND d.refobjid = m.objid AND d.deptype = 'n' \
                 JOIN local_objects l ON l.classid = d.classid AND l.objid = d.objid \
                 WHERE m.refclassid = 'pg_catalog.pg_extension'::regclass AND m.refobjid = e.oid AND m.deptype = 'e')) \
             ORDER BY e.oid"
        ),
        usize::MAX,
        None,
    )
    .await?;
    let extensions = extensions
        .rows
        .iter()
        .map(|row| {
            let text = texts(row);
            /*
             * An extension outside the namespace is referenced by qualified
             * names, so it has to land in that same schema. One inside it
             * follows the namespace to wherever the export is imported.
             */
            let schema = if row.get(2).is_some_and(CellValue::is_truthy) {
                String::new()
            } else {
                format!(" WITH SCHEMA {}", text_at(&text, 1))
            };
            format!("CREATE EXTENSION IF NOT EXISTS {}{schema} CASCADE;\n", text_at(&text, 0))
        })
        .collect();

    let types = postgres::run(
        conn,
        &format!(
            "SELECT t.typname, t.typtype::text, \
             (SELECT string_agg(quote_literal(e.enumlabel), ', ' ORDER BY e.enumsortorder) \
                 FROM pg_catalog.pg_enum e WHERE e.enumtypid = t.oid), \
             format_type(t.typbasetype, t.typtypmod), t.typnotnull, t.typdefault, \
             (SELECT quote_ident(co.collname) FROM pg_catalog.pg_collation co WHERE co.oid = t.typcollation \
                 AND t.typcollation <> (SELECT b.typcollation FROM pg_catalog.pg_type b WHERE b.oid = t.typbasetype)), \
             (SELECT string_agg(format('CONSTRAINT %I %s', con.conname, pg_get_constraintdef(con.oid)), ' ' ORDER BY con.conname) \
                 FROM pg_catalog.pg_constraint con WHERE con.contypid = t.oid AND con.contype = 'c'), \
             (SELECT string_agg(format('%I %s', a.attname, format_type(a.atttypid, a.atttypmod)), ', ' ORDER BY a.attnum) \
                 FROM pg_catalog.pg_attribute a WHERE a.attrelid = t.typrelid AND a.attnum > 0 AND NOT a.attisdropped), \
             (SELECT format_type(r.rngsubtype, NULL) \
                 || CASE WHEN r.rngsubdiff <> 0 THEN ', SUBTYPE_DIFF = ' || r.rngsubdiff::regproc::text ELSE '' END \
                 FROM pg_catalog.pg_range r WHERE r.rngtypid = t.oid) \
             FROM pg_catalog.pg_type t \
             LEFT JOIN pg_catalog.pg_class c ON c.oid = t.typrelid \
             WHERE t.typnamespace = {ns_oid} AND (t.typtype IN ('e', 'd', 'r') OR (t.typtype = 'c' AND c.relkind = 'c')) \
             AND {} ORDER BY t.oid",
            created_directly("pg_type", "t.oid")
        ),
        usize::MAX,
        None,
    )
    .await?;
    let types = types
        .rows
        .iter()
        .filter_map(|row| {
            let text = texts(row);
            let name = quote_double(&text_at(&text, 0));
            let create = match text_at(&text, 1).as_str() {
                "e" => format!("CREATE TYPE {name} AS ENUM ({})", text_at(&text, 2)),
                "d" => {
                    let mut sql = format!("CREATE DOMAIN {name} AS {}", text_at(&text, 3));
                    if let Some(collation) = text.get(6).cloned().flatten() {
                        sql.push_str(&format!(" COLLATE {collation}"));
                    }
                    if let Some(default) = text.get(5).cloned().flatten() {
                        sql.push_str(&format!(" DEFAULT {default}"));
                    }
                    if row.get(4).is_some_and(CellValue::is_truthy) {
                        sql.push_str(" NOT NULL");
                    }
                    if let Some(checks) = text.get(7).cloned().flatten() {
                        sql.push_str(&format!(" {checks}"));
                    }
                    sql
                }
                "c" => format!("CREATE TYPE {name} AS ({})", text_at(&text, 8)),
                "r" => format!("CREATE TYPE {name} AS RANGE (SUBTYPE = {})", text_at(&text, 9)),
                _ => return None,
            };
            /*
             * Types outlive dropped tables and other tables may use them, so
             * one that already exists is kept rather than dropped.
             */
            Some(format!(
                "DO $recon$ BEGIN\n    {create};\nEXCEPTION WHEN duplicate_object THEN NULL;\nEND $recon$;\n"
            ))
        })
        .collect();

    let functions = postgres::run(
        conn,
        &format!(
            "SELECT p.oid::text, pg_get_functiondef(p.oid), quote_ident(n.nspname), p.oid::regprocedure::text, EXISTS ( \
                 SELECT 1 FROM pg_catalog.pg_depend d \
                 JOIN pg_catalog.pg_type t ON t.oid = d.refobjid \
                 LEFT JOIN pg_catalog.pg_type e ON e.oid = t.typelem \
                 JOIN pg_catalog.pg_class r ON r.oid = CASE WHEN t.typrelid <> 0 THEN t.typrelid ELSE e.typrelid END \
                 WHERE d.classid = 'pg_catalog.pg_proc'::regclass AND d.objid = p.oid \
                 AND d.refclassid = 'pg_catalog.pg_type'::regclass AND r.relkind <> 'c') \
             FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace \
             WHERE p.pronamespace = {ns_oid} AND p.prokind IN ('f', 'p') AND {} ORDER BY p.oid",
            created_directly("pg_proc", "p.oid")
        ),
        usize::MAX,
        None,
    )
    .await?;
    let functions = functions
        .rows
        .iter()
        .map(|row| {
            let text = texts(row);
            PgFunction {
                oid: text_at(&text, 0),
                signature: text_at(&text, 3),
                definition: unqualify_function(&text_at(&text, 1), &text_at(&text, 2)),
                after_tables: row.get(4).is_some_and(CellValue::is_truthy),
            }
        })
        .collect();

    let uses = postgres::run(
        conn,
        &format!(
            "SELECT DISTINCT d.refobjid::text, c.relname, \
             CASE WHEN d.classid = 'pg_catalog.pg_proc'::regclass THEN d.objid::text END \
             FROM pg_catalog.pg_depend d \
             JOIN pg_catalog.pg_proc p ON p.oid = d.refobjid \
             LEFT JOIN pg_catalog.pg_attrdef ad ON d.classid = 'pg_catalog.pg_attrdef'::regclass AND ad.oid = d.objid \
             LEFT JOIN pg_catalog.pg_constraint con ON d.classid = 'pg_catalog.pg_constraint'::regclass AND con.oid = d.objid \
             LEFT JOIN pg_catalog.pg_trigger tg ON d.classid = 'pg_catalog.pg_trigger'::regclass AND tg.oid = d.objid \
             LEFT JOIN pg_catalog.pg_rewrite w ON d.classid = 'pg_catalog.pg_rewrite'::regclass AND w.oid = d.objid \
             LEFT JOIN pg_catalog.pg_index i ON d.classid = 'pg_catalog.pg_class'::regclass AND i.indexrelid = d.objid \
             LEFT JOIN pg_catalog.pg_type ty ON ty.oid = CASE WHEN d.classid = 'pg_catalog.pg_type'::regclass \
                 THEN d.objid ELSE con.contypid END \
             LEFT JOIN pg_catalog.pg_class c ON c.oid = COALESCE(ad.adrelid, NULLIF(con.conrelid, 0), tg.tgrelid, \
                 w.ev_class, i.indrelid) \
             WHERE d.refclassid = 'pg_catalog.pg_proc'::regclass AND p.pronamespace = {ns_oid} \
             AND (c.relnamespace = p.pronamespace OR ty.typnamespace = p.pronamespace \
                 OR d.classid = 'pg_catalog.pg_proc'::regclass)"
        ),
        usize::MAX,
        None,
    )
    .await?;
    let uses = uses
        .text_rows()
        .into_iter()
        .map(|row| PgFunctionUse {
            function: text_at(&row, 0),
            relation: row.get(1).cloned().flatten(),
            caller: row.get(2).cloned().flatten(),
        })
        .collect();

    Ok(PgObjects {
        extensions,
        types,
        functions,
        uses,
    })
}

/**
 * pg_get_functiondef always qualifies the function's own name with its
 * schema, which would pin the import to a schema of the same name.
 */
fn unqualify_function(definition: &str, schema: &str) -> String {
    let (header, body) = definition.split_at(definition.find('\n').unwrap_or(definition.len()));
    let header = header
        .replacen(&format!(" FUNCTION {schema}."), " FUNCTION ", 1)
        .replacen(&format!(" PROCEDURE {schema}."), " PROCEDURE ", 1);
    format!("{header}{body}")
}

/**
 * The functions to export: all of them, or only those the chosen relations
 * and the namespace's types use, along with the functions those call.
 */
fn wanted_functions(objects: &PgObjects, chosen: &HashSet<&str>, all: bool) -> HashSet<String> {
    if all {
        return objects.functions.iter().map(|function| function.oid.clone()).collect();
    }
    let mut wanted: HashSet<String> = objects
        .uses
        .iter()
        .filter(|used| match (&used.relation, &used.caller) {
            (Some(relation), _) => chosen.contains(relation.as_str()),
            (None, None) => true,
            (None, Some(_)) => false,
        })
        .map(|used| used.function.clone())
        .collect();
    loop {
        let called: Vec<String> = objects
            .uses
            .iter()
            .filter(|used| used.caller.as_ref().is_some_and(|caller| wanted.contains(caller)))
            .filter(|used| !wanted.contains(&used.function))
            .map(|used| used.function.clone())
            .collect();
        if called.is_empty() {
            return wanted;
        }
        wanted.extend(called);
    }
}

fn postgres_comments(name: &str, relation: &PgRelation) -> String {
    let quoted = quote_double(name);
    let kind = match relation.kind.as_str() {
        "v" => "VIEW",
        "m" => "MATERIALIZED VIEW",
        _ => "TABLE",
    };
    let mut sql = String::new();
    if let Some(comment) = &relation.comment {
        sql.push_str(&format!("COMMENT ON {kind} {quoted} IS {};\n", quote_literal(comment)));
    }
    for (column, comment) in &relation.column_comments {
        sql.push_str(&format!(
            "COMMENT ON COLUMN {quoted}.{} IS {};\n",
            quote_double(column),
            quote_literal(comment)
        ));
    }
    sql
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
    let views = dependency_order(
        selected.iter().copied().filter(|(_, relation)| is_view(relation)).collect(),
        |(_, relation), (other, _)| relation.reads.contains(other),
    );
    let exported: Vec<(&String, &PgRelation)> = selected
        .iter()
        .copied()
        .filter(|(_, relation)| !is_view(relation) && unsupported(relation).is_none())
        .collect();

    /*
     * Sequences are created up front because a column default can draw from
     * one that another table owns. Borrowed sequences no exported table owns
     * get their position set along with the first table that uses them.
     */
    let owned: HashSet<&str> = exported
        .iter()
        .flat_map(|(_, relation)| relation.sequences.iter())
        .filter(|sequence| !sequence.identity)
        .map(|sequence| sequence.name.as_str())
        .collect();
    let mut sequences: Vec<&PgSequence> = Vec::new();
    for (_, relation) in &exported {
        for sequence in relation.sequences.iter().chain(&relation.borrowed) {
            if !sequence.identity && !sequences.iter().any(|known| known.name == sequence.name) {
                sequences.push(sequence);
            }
        }
    }
    let mut positioned: HashSet<&str> = HashSet::new();

    let objects = if options.structure { Some(postgres_objects(conn, namespace).await?) } else { None };
    let (early_functions, late_functions): (Vec<&PgFunction>, Vec<&PgFunction>) = match &objects {
        Some(objects) => {
            let wanted_functions = wanted_functions(objects, &wanted, options.routines);
            objects
                .functions
                .iter()
                .filter(|function| wanted_functions.contains(&function.oid))
                .partition(|function| !function.after_tables)
        }
        None => Default::default(),
    };
    let function_sql = |functions: &[&PgFunction]| -> String {
        functions
            .iter()
            .map(|function| format!("{};\n", function.definition.trim_end().trim_end_matches(';')))
            .collect()
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
        for (name, relation) in views.iter().rev() {
            let kind = if relation.kind == "m" { "MATERIALIZED VIEW" } else { "VIEW" };
            drops.push_str(&format!("DROP {kind} IF EXISTS {};\n", quote_double(name)));
        }
        for function in &late_functions {
            drops.push_str(&format!("DROP ROUTINE IF EXISTS {};\n", function.signature));
        }
        /*
         * One statement for every table, so defaults and sequences shared
         * between them don't block dropping them one at a time.
         */
        if !exported.is_empty() {
            let names: Vec<String> = exported.iter().map(|(name, _)| quote_double(name)).collect();
            drops.push_str(&format!("DROP TABLE IF EXISTS {};\n", names.join(", ")));
        }
        dump.write(&drops)?;
    }

    if let Some(objects) = &objects {
        if !objects.extensions.is_empty() {
            dump.write(&format!("\n-- Extensions\n{}", objects.extensions.concat()))?;
        }
        if !objects.types.is_empty() {
            dump.write(&format!("\n-- Types\n{}", objects.types.concat()))?;
        }
        if !early_functions.is_empty() {
            dump.write(&format!("\n-- Functions\n{}", function_sql(&early_functions)))?;
        }
        if !sequences.is_empty() {
            let created: String = sequences
                .iter()
                .map(|sequence| format!("CREATE SEQUENCE IF NOT EXISTS {}{};\n", quote_double(&sequence.name), sequence.options))
                .collect();
            dump.write(&format!("\n-- Sequences\n{created}"))?;
        }
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
            sql.push_str(&postgres_comments(name, relation));
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
                if let Some((last, called)) = postgres_sequence_position(conn, sequence).await? {
                    dump.write(&format!(
                        "SELECT pg_catalog.setval(pg_catalog.pg_get_serial_sequence({}, {}), {last}, {called});\n",
                        quote_literal(&quoted),
                        quote_literal(&sequence.column)
                    ))?;
                }
            }
            for sequence in &relation.borrowed {
                if owned.contains(sequence.name.as_str()) || !positioned.insert(sequence.name.as_str()) {
                    continue;
                }
                if let Some((last, called)) = postgres_sequence_position(conn, sequence).await? {
                    dump.write(&format!(
                        "SELECT pg_catalog.setval({}, {last}, {called});\n",
                        quote_literal(&quote_double(&sequence.name))
                    ))?;
                }
            }
        }
        if options.structure && !relation.indexes.is_empty() {
            dump.write(&relation.indexes.iter().map(|index| format!("{index};\n")).collect::<String>())?;
        }
    }

    if options.structure {
        if !late_functions.is_empty() {
            dump.write(&format!("\n-- Functions using table row types\n{}", function_sql(&late_functions)))?;
        }
        for (name, relation) in &views {
            dump.start(name)?;
            let definition = relation.view.as_deref().unwrap_or_default().trim().trim_end_matches(';');
            let mut sql = if relation.kind == "m" {
                let mut sql = format!("CREATE MATERIALIZED VIEW {} AS\n{definition}\nWITH DATA;\n", quote_double(name));
                for index in &relation.indexes {
                    sql.push_str(&format!("{index};\n"));
                }
                sql
            } else {
                format!("CREATE VIEW {} AS\n{definition};\n", quote_double(name))
            };
            sql.push_str(&postgres_comments(name, relation));
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

        /*
         * Triggers come after the data so the import doesn't fire them on
         * rows that already went through them once.
         */
        let mut triggers = String::new();
        for (name, relation) in exported.iter().chain(&views) {
            for trigger in &relation.triggers {
                triggers.push_str(&format!("{};\n", trigger.definition));
                let state = match trigger.enabled.as_str() {
                    "D" => "DISABLE",
                    "R" => "ENABLE REPLICA",
                    "A" => "ENABLE ALWAYS",
                    _ => continue,
                };
                if !is_view(relation) {
                    triggers.push_str(&format!("ALTER TABLE {} {state} TRIGGER {};\n", quote_double(name), trigger.name));
                }
            }
        }
        if !triggers.is_empty() {
            dump.write(&format!("\n-- Triggers\n{triggers}"))?;
        }
    }
    postgres::run(conn, "COMMIT", 0, None).await?;
    Ok(())
}

/// The sequence's `last_value` and `is_called`, for a `setval` that puts it back where it was.
async fn postgres_sequence_position(conn: &mut PgConnection, sequence: &PgSequence) -> Result<Option<(String, bool)>, String> {
    let output = postgres::run(conn, &format!("SELECT last_value, is_called FROM {}", sequence.qualified), 1, None).await?;
    Ok(output.rows.first().map(|row| {
        let last = row.first().and_then(CellValue::to_text).unwrap_or_else(|| "1".into());
        (last, row.get(1).is_some_and(CellValue::is_truthy))
    }))
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
        assert_eq!(
            strip_definer("CREATE DEFINER=`root`@`localhost` TRIGGER `t` BEFORE INSERT ON `x` FOR EACH ROW SET NEW.a = 1"),
            "CREATE TRIGGER `t` BEFORE INSERT ON `x` FOR EACH ROW SET NEW.a = 1"
        );
        assert_eq!(
            strip_definer("CREATE DEFINER=`app`@`%` PROCEDURE `p`()\n    SQL SECURITY INVOKER\nBEGIN SELECT 1; END"),
            "CREATE PROCEDURE `p`()\n    SQL SECURITY INVOKER\nBEGIN SELECT 1; END"
        );
        assert_eq!(strip_definer("CREATE VIEW v AS select 1"), "CREATE VIEW v AS select 1");
    }

    #[test]
    fn orders_items_after_their_dependencies() {
        let deps = |item: &&str, other: &&str| matches!((*item, *other), ("a", "c") | ("c", "b"));
        assert_eq!(dependency_order(vec!["a", "b", "c", "d"], deps), vec!["b", "c", "a", "d"]);
        let cycle = |item: &&str, other: &&str| matches!((*item, *other), ("a", "b") | ("b", "a"));
        assert_eq!(dependency_order(vec!["a", "b", "c"], cycle), vec!["c", "a", "b"]);
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
        let options = DumpOptions { structure: true, data: true, drop_tables: true, routines: true };
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
            "CREATE FUNCTION shout(s TEXT) RETURNS TEXT DETERMINISTIC RETURN UPPER(s)",
            "CREATE PROCEDURE bump(IN uid INT) BEGIN UPDATE users SET ratio = COALESCE(ratio, 0) + 1 WHERE id = uid; SELECT 1; END",
            "CREATE VIEW a_loud AS SELECT shout(name) AS name FROM names",
            "CREATE TRIGGER count_post AFTER INSERT ON posts FOR EACH ROW \
             BEGIN UPDATE users SET note = CONCAT(COALESCE(note, ''), '+') WHERE id = NEW.user_id; END",
        ] {
            conn.execute(sql).await.unwrap_or_else(|err| panic!("{err}: {sql}"));
        }

        let tables = [
            DumpTable { name: "a_loud".into(), view: true },
            DumpTable { name: "names".into(), view: true },
            DumpTable { name: "posts".into(), view: false },
            DumpTable { name: "users".into(), view: false },
        ];
        let mut out: Vec<u8> = Vec::new();
        let cancel = AtomicBool::new(false);
        let mut progress = |_: DumpProgress| {};
        let mut dump = Dump::new(&mut out, &cancel, &mut progress);
        let options = DumpOptions { structure: true, data: true, drop_tables: true, routines: true };
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

        import(&mut conn, sql.clone().into_bytes()).await;
        assert_eq!(conn.run(compare, 10, None).await.unwrap().text_rows(), expected);
        let loud = conn.run("SELECT name FROM a_loud ORDER BY name", 10, None).await.unwrap().text_rows();
        assert_eq!(loud[1][0].as_deref(), Some("ZERO"));
        let routines = conn
            .run("SELECT COUNT(*) FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA = 'recon_dump_dst'", 1, None)
            .await
            .unwrap();
        assert_eq!(routines.rows[0][0].as_i64(), Some(2));
        conn.execute("INSERT INTO posts VALUES (2, 5, 'fires the trigger')").await.unwrap();
        let note = conn.run("SELECT note FROM users WHERE id = 5", 1, None).await.unwrap();
        assert_eq!(note.text_rows()[0][0].as_deref(), Some("日本語 🎉+"));

        for sql in [
            "ALTER DATABASE recon_dump_dst CHARACTER SET latin1 COLLATE latin1_swedish_ci",
            "USE recon_dump_dst",
            "CREATE EVENT keep_me ON SCHEDULE EVERY 1 DAY DO SELECT 1",
            "CREATE TABLE stray (a INT)",
            "CREATE VIEW stray_view AS SELECT a FROM stray",
            "CREATE PROCEDURE stray_proc() SELECT 1",
            "DELETE FROM posts",
        ] {
            conn.execute(sql).await.unwrap_or_else(|err| panic!("{err}: {sql}"));
        }
        crate::db::restore::reset_namespace(&mut conn, Driver::Mysql, "recon_dump_dst").await.unwrap();
        let leftovers = conn
            .run(
                "SELECT (SELECT COUNT(*) FROM information_schema.TABLES WHERE TABLE_SCHEMA = 'recon_dump_dst') + \
                 (SELECT COUNT(*) FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA = 'recon_dump_dst')",
                1,
                None,
            )
            .await
            .unwrap();
        assert_eq!(leftovers.rows[0][0].as_i64(), Some(0));
        import(&mut conn, sql.into_bytes()).await;
        assert_eq!(conn.run(compare, 10, None).await.unwrap().text_rows(), expected);
        let kept = conn
            .run(
                "SELECT (SELECT COUNT(*) FROM information_schema.EVENTS WHERE EVENT_SCHEMA = 'recon_dump_dst'), \
                 (SELECT DEFAULT_CHARACTER_SET_NAME FROM information_schema.SCHEMATA WHERE SCHEMA_NAME = 'recon_dump_dst'), \
                 (SELECT COUNT(*) FROM information_schema.TABLES WHERE TABLE_SCHEMA = 'recon_dump_dst' \
                     AND TABLE_NAME IN ('stray', 'stray_view'))",
                1,
                None,
            )
            .await
            .unwrap()
            .text_rows();
        assert_eq!(kept[0], vec![Some("1".into()), Some("latin1".into()), Some("0".into())]);

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

    async fn open_postgres(port: u16, database: &str) -> crate::db::Opened {
        let entry: crate::models::ConnectionEntry = serde_json::from_value(serde_json::json!({
            "id": "live", "name": "live", "driver": "postgres", "host": "127.0.0.1", "port": port,
            "user": "postgres", "database": database, "filePath": "", "sslMode": "prefer", "headerColor": "",
            "savePassword": false,
        }))
        .unwrap();
        postgres::open(&entry, None).await.unwrap()
    }

    async fn export(conn: &mut Conn, namespace: &str, tables: &[DumpTable], options: DumpOptions) -> String {
        let mut out: Vec<u8> = Vec::new();
        let cancel = AtomicBool::new(false);
        let mut progress = |_: DumpProgress| {};
        let mut dump = Dump::new(&mut out, &cancel, &mut progress);
        run(conn, "live", namespace, tables, options, &mut dump).await.unwrap();
        String::from_utf8(out).unwrap()
    }

    async fn import_postgres(conn: &mut Conn, schema: &str, sql: &str) {
        conn.execute(&format!("SET search_path TO {schema}, public")).await.unwrap();
        for statement in split_all(Driver::Postgres, sql.as_bytes()) {
            conn.execute(&statement.sql)
                .await
                .unwrap_or_else(|err| panic!("line {}: {err}: {}", statement.line, statement.sql));
        }
    }

    /**
     * Exports a schema from a scratch Postgres database and imports it into
     * a differently named schema in a fresh database:
     * `RECON_TEST_POSTGRES_PORT=5432 cargo test -- --ignored`.
     */
    #[tokio::test]
    #[ignore]
    async fn live_postgres_round_trip() {
        let Some(port) = std::env::var("RECON_TEST_POSTGRES_PORT").ok().and_then(|port| port.parse().ok()) else {
            return;
        };
        let admin = open_postgres(port, "postgres").await;
        let mut admin_conn = admin.conn;
        for sql in [
            "DROP DATABASE IF EXISTS recon_dump_src",
            "DROP DATABASE IF EXISTS recon_dump_dst",
            "DROP ROLE IF EXISTS recon_dump_user",
            "CREATE ROLE recon_dump_user",
            "CREATE DATABASE recon_dump_src",
            "CREATE DATABASE recon_dump_dst OWNER recon_dump_user",
        ] {
            admin_conn.execute(sql).await.unwrap_or_else(|err| panic!("{err}: {sql}"));
        }

        let source = open_postgres(port, "recon_dump_src").await;
        let mut src = source.conn;
        for sql in [
            "CREATE EXTENSION citext SCHEMA public",
            "CREATE SCHEMA app",
            "SET search_path TO app, public",
            "CREATE TYPE mood AS ENUM ('sad', 'ok', 'it''s fine')",
            "CREATE DOMAIN positive AS integer DEFAULT 1 NOT NULL CHECK (VALUE > 0)",
            "CREATE TYPE pair AS (a integer, b text)",
            "CREATE TYPE floatrange AS RANGE (subtype = float8, subtype_diff = float8mi)",
            "CREATE SEQUENCE tickets START 100",
            "CREATE FUNCTION shout(t text) RETURNS text LANGUAGE sql IMMUTABLE AS $$ SELECT upper(t) $$",
            "CREATE TABLE users (id serial PRIMARY KEY, email public.citext UNIQUE, feeling mood NOT NULL DEFAULT 'ok', \
             score positive, spot pair, span floatrange, ticket integer DEFAULT nextval('tickets'), \
             loud text GENERATED ALWAYS AS (shout(email::text)) STORED)",
            "COMMENT ON TABLE users IS 'People''s accounts'",
            "COMMENT ON COLUMN users.email IS 'Used to sign in'",
            "CREATE TABLE audit (id integer GENERATED ALWAYS AS IDENTITY, note text)",
            "CREATE TABLE extras (id integer DEFAULT nextval('users_id_seq'), label text)",
            "CREATE FUNCTION log_user() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN INSERT INTO audit (note) VALUES (NEW.email); RETURN NEW; END $$",
            "CREATE TRIGGER users_log AFTER INSERT ON users FOR EACH ROW EXECUTE FUNCTION log_user()",
            "CREATE TRIGGER users_off BEFORE UPDATE ON users FOR EACH ROW EXECUTE FUNCTION log_user()",
            "ALTER TABLE users DISABLE TRIGGER users_off",
            "CREATE FUNCTION everyone() RETURNS SETOF users LANGUAGE sql AS $$ SELECT * FROM users $$",
            "CREATE VIEW a_loud AS SELECT 'x'::text AS email",
            "CREATE VIEW b_names AS SELECT email FROM users",
            "CREATE OR REPLACE VIEW a_loud AS SELECT shout(email::text) AS email FROM b_names",
            "COMMENT ON VIEW a_loud IS 'Shouting'",
            "INSERT INTO users (email, feeling, score, spot, span) VALUES \
             ('Ann@x.io', 'it''s fine', 3, ROW(1, 'x'), '[1.5,2.5)'), ('bob@x.io', 'sad', 1, NULL, NULL)",
            "INSERT INTO extras (label) VALUES ('borrowed')",
        ] {
            src.execute(sql).await.unwrap_or_else(|err| panic!("{err}: {sql}"));
        }

        let relation = |name: &str, view: bool| DumpTable { name: name.into(), view };
        let tables = [
            relation("a_loud", true),
            relation("audit", false),
            relation("b_names", true),
            relation("extras", false),
            relation("users", false),
        ];
        let options = DumpOptions { structure: true, data: true, drop_tables: true, routines: true };
        let sql = export(&mut src, "app", &tables, options).await;
        assert!(!sql.contains("app."), "{sql}");
        assert!(!sql.contains("LANGUAGE internal"), "{sql}");
        assert!(sql.contains("CREATE EXTENSION IF NOT EXISTS citext WITH SCHEMA public CASCADE;"), "{sql}");

        let compare = "SELECT id, email::text, feeling::text, score, spot::text, span::text, ticket, loud FROM users ORDER BY id";
        let expected = src.run(compare, 10, None).await.unwrap().text_rows();

        let target = open_postgres(port, "recon_dump_dst").await;
        let mut dst = target.conn;
        dst.execute("SET ROLE recon_dump_user").await.unwrap();
        dst.execute("CREATE SCHEMA moved").await.unwrap();
        import_postgres(&mut dst, "moved", &sql).await;
        assert_eq!(dst.run(compare, 10, None).await.unwrap().text_rows(), expected);
        let audited = dst.run("SELECT count(*) FROM audit", 1, None).await.unwrap();
        assert_eq!(audited.rows[0][0].as_i64(), Some(2));

        import_postgres(&mut dst, "moved", &sql).await;
        assert_eq!(dst.run(compare, 10, None).await.unwrap().text_rows(), expected);

        dst.execute("INSERT INTO users (email) VALUES ('cy@x.io')").await.unwrap();
        let added = dst.run("SELECT id, ticket, feeling::text, score FROM users WHERE email = 'CY@X.IO'", 1, None).await.unwrap();
        assert_eq!(
            added.text_rows()[0],
            vec![Some("4".into()), Some("102".into()), Some("ok".into()), Some("1".into())]
        );
        let audited = dst.run("SELECT count(*) FROM audit", 1, None).await.unwrap();
        assert_eq!(audited.rows[0][0].as_i64(), Some(3));
        let disabled = dst.run("SELECT tgenabled::text FROM pg_trigger WHERE tgname = 'users_off'", 1, None).await.unwrap();
        assert_eq!(disabled.text_rows()[0][0].as_deref(), Some("D"));
        let loud = dst.run("SELECT email FROM a_loud ORDER BY email", 10, None).await.unwrap();
        assert_eq!(loud.text_rows()[0][0].as_deref(), Some("ANN@X.IO"));
        let everyone = dst.run("SELECT count(*) FROM everyone()", 1, None).await.unwrap();
        assert_eq!(everyone.rows[0][0].as_i64(), Some(3));
        let comments = dst
            .run(
                "SELECT obj_description('moved.users'::regclass, 'pg_class'), \
                 col_description('moved.users'::regclass, 2), obj_description('moved.a_loud'::regclass, 'pg_class')",
                1,
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            comments.text_rows()[0],
            vec![Some("People's accounts".into()), Some("Used to sign in".into()), Some("Shouting".into())]
        );

        let subset = export(&mut src, "app", &[relation("extras", false)], DumpOptions { routines: false, ..options }).await;
        assert!(!subset.contains("everyone"), "{subset}");
        assert!(subset.contains("CREATE SEQUENCE IF NOT EXISTS \"users_id_seq\""), "{subset}");
        dst.execute("CREATE SCHEMA subset").await.unwrap();
        import_postgres(&mut dst, "subset", &subset).await;
        dst.execute("INSERT INTO extras (label) VALUES ('next')").await.unwrap();
        let ids = dst.run("SELECT id FROM extras ORDER BY id", 10, None).await.unwrap();
        assert_eq!(ids.text_rows(), vec![vec![Some("3".into())], vec![Some("4".into())]]);

        async fn restore(conn: &mut Conn, sql: &str) -> Result<(), String> {
            conn.execute("BEGIN").await?;
            let ran = async {
                crate::db::restore::reset_namespace(conn, Driver::Postgres, "moved").await?;
                for statement in split_all(Driver::Postgres, sql.as_bytes()) {
                    conn.execute(&statement.sql).await?;
                }
                conn.execute("COMMIT").await.map(|_| ())
            }
            .await;
            if ran.is_err() {
                conn.execute("ROLLBACK").await.unwrap();
            }
            ran
        }
        let count = "SELECT count(*) FROM moved.users";
        let before = dst.run(count, 1, None).await.unwrap().text_rows();

        dst.execute("CREATE SCHEMA outside").await.unwrap();
        dst.execute("CREATE VIEW outside.peek AS SELECT id FROM moved.users").await.unwrap();
        let err = restore(&mut dst, &sql).await.unwrap_err();
        assert!(err.contains("outside.peek"), "{err}");
        assert_eq!(dst.run(count, 1, None).await.unwrap().text_rows(), before);
        dst.execute("DROP SCHEMA outside CASCADE").await.unwrap();

        let err = restore(&mut dst, &format!("{sql}SELECT 1 / 0;\n")).await.unwrap_err();
        assert!(err.contains("division by zero"), "{err}");
        assert_eq!(dst.run(count, 1, None).await.unwrap().text_rows(), before);

        restore(&mut dst, &sql).await.unwrap();
        dst.execute("SET search_path TO moved, public").await.unwrap();
        assert_eq!(dst.run(compare, 10, None).await.unwrap().text_rows(), expected);
        let owner = dst
            .run("SELECT pg_get_userbyid(nspowner) FROM pg_namespace WHERE nspname = 'moved'", 1, None)
            .await
            .unwrap();
        assert_eq!(owner.text_rows()[0][0].as_deref(), Some("recon_dump_user"));

        src.close().await;
        source.pool.close().await;
        dst.close().await;
        target.pool.close().await;
        for sql in ["DROP DATABASE recon_dump_src", "DROP DATABASE recon_dump_dst", "DROP ROLE recon_dump_user"] {
            admin_conn.execute(sql).await.unwrap();
        }
        admin_conn.close().await;
        admin.pool.close().await;
    }

    #[tokio::test]
    async fn exports_only_the_chosen_parts() {
        let mut conn = SqliteConnection::connect("sqlite::memory:").await.unwrap();
        sqlite::run(&mut conn, "CREATE TABLE t (a INT)", 0, None).await.unwrap();
        sqlite::run(&mut conn, "INSERT INTO t VALUES (1)", 0, None).await.unwrap();
        let tables = [DumpTable { name: "t".into(), view: false }];
        let (data_only, _) = export_sqlite(&mut conn, &tables, DumpOptions { structure: false, data: true, drop_tables: true, routines: true }).await;
        assert!(!data_only.contains("CREATE TABLE") && !data_only.contains("DROP TABLE"));
        assert!(data_only.contains("INSERT INTO \"t\" (\"a\") VALUES\n(1);"));
        let (structure_only, _) = export_sqlite(&mut conn, &tables, DumpOptions { structure: true, data: false, drop_tables: false, routines: true }).await;
        assert!(structure_only.contains("CREATE TABLE t (a INT);") && !structure_only.contains("INSERT"));
    }
}
