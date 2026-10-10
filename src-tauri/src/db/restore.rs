use std::io::{BufRead, Read};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use super::dump::mysql_literal;
use super::{dialect, first_text, quote_backtick, quote_double, quote_literal, text_at, CellValue, Conn};
use crate::models::Driver;

pub const BACKUP_VERSION: u32 = 1;
const MARKER: &str = "-- Recon backup v";
const NOT_A_BACKUP: &str = "Not a Recon backup. Use Import for plain .sql files.";
const MAX_HEADER_LINES: usize = 1000;
const MAX_LINE: u64 = 64 * 1024;
const DEPENDENTS_SHOWN: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupInfo {
    pub version: u32,
    pub driver: Driver,
    pub namespace: String,
    pub server: String,
    pub created_at: String,
    /// Tables and routines the dump left out, each with the reason.
    pub skipped: Vec<String>,
}

fn driver_name(driver: Driver) -> &'static str {
    match driver {
        Driver::Mysql => "mysql",
        Driver::Postgres => "postgres",
        Driver::Sqlite => "sqlite",
    }
}

fn parse_driver(name: &str) -> Option<Driver> {
    match name {
        "mysql" => Some(Driver::Mysql),
        "postgres" => Some(Driver::Postgres),
        "sqlite" => Some(Driver::Sqlite),
        _ => None,
    }
}

fn one_line(value: &str) -> String {
    value.replace(['\r', '\n'], " ")
}

/// The comment block a backup starts with, ending in a blank line so it never runs into the dump's own comments.
pub fn header(info: &BackupInfo) -> String {
    let mut text = format!(
        "{MARKER}{}\n-- Driver: {}\n-- Namespace: {}\n-- Server: {}\n-- Created: {}\n",
        info.version,
        driver_name(info.driver),
        one_line(&info.namespace),
        one_line(&info.server),
        one_line(&info.created_at),
    );
    for skipped in &info.skipped {
        text.push_str(&format!("-- Skipped: {}\n", one_line(skipped)));
    }
    text.push('\n');
    text
}

fn next_line(reader: &mut dyn BufRead) -> Result<Option<String>, String> {
    let mut line = Vec::new();
    let read = Read::take(&mut *reader, MAX_LINE)
        .read_until(b'\n', &mut line)
        .map_err(|_| NOT_A_BACKUP.to_string())?;
    if read == 0 {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&line).trim_end_matches(['\r', '\n']).to_string()))
}

/// Reads only the header block, so checking a large backup never scans the whole file.
pub fn parse_header(reader: &mut dyn BufRead) -> Result<BackupInfo, String> {
    let first = next_line(reader)?.ok_or(NOT_A_BACKUP)?;
    let version: u32 = first
        .strip_prefix(MARKER)
        .and_then(|version| version.trim().parse().ok())
        .ok_or(NOT_A_BACKUP)?;
    if version > BACKUP_VERSION {
        return Err("This backup was made by a newer version of Recon.".into());
    }
    let mut driver = None;
    let mut info = BackupInfo {
        version,
        driver: Driver::Sqlite,
        namespace: String::new(),
        server: String::new(),
        created_at: String::new(),
        skipped: Vec::new(),
    };
    for _ in 0..MAX_HEADER_LINES {
        let Some(line) = next_line(reader)? else {
            break;
        };
        let Some((key, value)) = line.strip_prefix("-- ").and_then(|rest| rest.split_once(": ")) else {
            break;
        };
        match key {
            "Driver" => driver = parse_driver(value),
            "Namespace" => info.namespace = value.to_string(),
            "Server" => info.server = value.to_string(),
            "Created" => info.created_at = value.to_string(),
            "Skipped" => info.skipped.push(value.to_string()),
            _ => {}
        }
    }
    info.driver = driver.ok_or("This backup doesn't say which kind of database it came from.")?;
    Ok(info)
}

/// The current time as an RFC 3339 UTC timestamp.
pub fn utc_timestamp() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|elapsed| elapsed.as_secs()).unwrap_or(0);
    let (days, rest) = (secs / 86_400, secs % 86_400);
    // Howard Hinnant's days-to-civil conversion.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/**
 * Clears `namespace` of every kind of object a backup recreates, and leaves
 * the connection pointed at it. On Postgres this is meant to run inside the
 * restore's transaction.
 */
pub async fn reset_namespace(conn: &mut Conn, driver: Driver, namespace: &str) -> Result<(), String> {
    match driver {
        Driver::Mysql => reset_mysql(conn, namespace).await,
        Driver::Postgres => reset_postgres(conn, namespace).await,
        Driver::Sqlite => reset_sqlite(conn, namespace).await,
    }
}

/**
 * Empties the database in place rather than dropping it, which keeps its
 * charset, collation, and events, and needs no rights on the database itself.
 */
async fn reset_mysql(conn: &mut Conn, namespace: &str) -> Result<(), String> {
    let schema = mysql_literal(&CellValue::Text(namespace.to_string()));
    let qualified = |name: &str| format!("{}.{}", quote_backtick(namespace), quote_backtick(name));
    refuse_dependents(
        namespace,
        mysql_dependents(conn, namespace, &schema).await?,
        false,
        "Restoring could leave these broken or pointing at rows that no longer exist.",
    )?;
    conn.execute("SET @recon_foreign_key_checks = @@FOREIGN_KEY_CHECKS, FOREIGN_KEY_CHECKS = 0").await?;
    let result = async {
        let relations = conn
            .run(
                &format!("SELECT TABLE_NAME, TABLE_TYPE FROM information_schema.TABLES WHERE TABLE_SCHEMA = {schema}"),
                usize::MAX,
                None,
            )
            .await?
            .text_rows();
        let (views, tables): (Vec<_>, Vec<_>) = relations.iter().partition(|row| text_at(row, 1) == "VIEW");
        for (kind, rows) in [("VIEW", views), ("TABLE", tables)] {
            if !rows.is_empty() {
                let names: Vec<String> = rows.iter().map(|row| qualified(&text_at(row, 0))).collect();
                conn.execute(&format!("DROP {kind} IF EXISTS {}", names.join(", "))).await?;
            }
        }
        let routines = conn
            .run(
                &format!(
                    "SELECT ROUTINE_TYPE, ROUTINE_NAME FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA = {schema}"
                ),
                usize::MAX,
                None,
            )
            .await?
            .text_rows();
        for row in &routines {
            conn.execute(&format!("DROP {} IF EXISTS {}", text_at(row, 0), qualified(&text_at(row, 1))))
                .await?;
        }
        conn.execute(&format!("USE {}", quote_backtick(namespace))).await?;
        Ok::<_, String>(())
    }
    .await;
    let restored = conn.execute("SET FOREIGN_KEY_CHECKS = @recon_foreign_key_checks").await;
    result.and(restored.map(|_| ()))
}

/**
 * Foreign keys and views in other databases that use this one. A stored view
 * definition always qualifies names as `db`.`name`, which catches tables and
 * functions alike, where `VIEW_ROUTINE_USAGE` misses functions in other
 * databases. Views the user can't read the definition of, and triggers and
 * routines elsewhere, can't be checked.
 */
async fn mysql_dependents(conn: &mut Conn, namespace: &str, schema: &str) -> Result<Vec<String>, String> {
    let keys = format!(
        "SELECT CONCAT('foreign key ', CONSTRAINT_NAME, ' on ', CONSTRAINT_SCHEMA, '.', TABLE_NAME) \
         FROM information_schema.REFERENTIAL_CONSTRAINTS \
         WHERE UNIQUE_CONSTRAINT_SCHEMA = {schema} AND CONSTRAINT_SCHEMA <> {schema}"
    );
    let mut names: Vec<String> = conn.run(&keys, usize::MAX, None).await?.text_rows().iter().map(|row| text_at(row, 0)).collect();
    let needle = mysql_literal(&CellValue::Text(format!("{}.", quote_backtick(namespace))));
    let views = format!(
        "SELECT CONCAT('view ', TABLE_SCHEMA, '.', TABLE_NAME) FROM information_schema.VIEWS \
         WHERE TABLE_SCHEMA <> {schema} AND LOCATE({needle}, VIEW_DEFINITION) > 0"
    );
    names.extend(conn.run(&views, usize::MAX, None).await?.text_rows().iter().map(|row| text_at(row, 0)));
    names.sort();
    names.dedup();
    Ok(names)
}

/// Fails with the first few `names` when anything outside the namespace depends on it.
fn refuse_dependents(namespace: &str, mut names: Vec<String>, more: bool, consequence: &str) -> Result<(), String> {
    if names.is_empty() {
        return Ok(());
    }
    let more = more || names.len() > DEPENDENTS_SHOWN;
    names.truncate(DEPENDENTS_SHOWN);
    let verb = if names.len() == 1 && !more { "depends" } else { "depend" };
    let rest = if more { ", and more" } else { "" };
    Err(format!("Can't restore into “{namespace}”: {}{rest} {verb} on it. {consequence}", names.join(", ")))
}

/**
 * Objects outside the schema that depend on something in it, which
 * `DROP SCHEMA ... CASCADE` would otherwise remove without a word. Extensions
 * installed in the schema count, since dropping one drops every column that
 * uses its types.
 */
fn postgres_dependents_sql(namespace: &str) -> String {
    let ns = quote_literal(namespace);
    let ns_oid = format!("(SELECT oid FROM pg_catalog.pg_namespace WHERE nspname = {ns})");
    format!(
        "WITH inside (classid, objid) AS ( \
             SELECT 'pg_catalog.pg_class'::regclass, c.oid FROM pg_catalog.pg_class c WHERE c.relnamespace = {ns_oid} \
             UNION ALL SELECT 'pg_catalog.pg_type'::regclass, t.oid FROM pg_catalog.pg_type t WHERE t.typnamespace = {ns_oid} \
             UNION ALL SELECT 'pg_catalog.pg_proc'::regclass, p.oid FROM pg_catalog.pg_proc p WHERE p.pronamespace = {ns_oid} \
             UNION ALL SELECT 'pg_catalog.pg_extension'::regclass, e.oid FROM pg_catalog.pg_extension e \
                 WHERE e.extnamespace = {ns_oid} \
         ), \
         dependents AS ( \
             SELECT d.classid, d.objid, d.objsubid, CASE d.classid \
                 WHEN 'pg_catalog.pg_class'::regclass THEN (SELECT relnamespace FROM pg_catalog.pg_class WHERE oid = d.objid) \
                 WHEN 'pg_catalog.pg_type'::regclass THEN (SELECT typnamespace FROM pg_catalog.pg_type WHERE oid = d.objid) \
                 WHEN 'pg_catalog.pg_proc'::regclass THEN (SELECT pronamespace FROM pg_catalog.pg_proc WHERE oid = d.objid) \
                 WHEN 'pg_catalog.pg_constraint'::regclass THEN \
                     (SELECT connamespace FROM pg_catalog.pg_constraint WHERE oid = d.objid) \
                 WHEN 'pg_catalog.pg_operator'::regclass THEN \
                     (SELECT oprnamespace FROM pg_catalog.pg_operator WHERE oid = d.objid) \
                 WHEN 'pg_catalog.pg_collation'::regclass THEN \
                     (SELECT collnamespace FROM pg_catalog.pg_collation WHERE oid = d.objid) \
                 WHEN 'pg_catalog.pg_rewrite'::regclass THEN (SELECT c.relnamespace FROM pg_catalog.pg_rewrite w \
                     JOIN pg_catalog.pg_class c ON c.oid = w.ev_class WHERE w.oid = d.objid) \
                 WHEN 'pg_catalog.pg_attrdef'::regclass THEN (SELECT c.relnamespace FROM pg_catalog.pg_attrdef a \
                     JOIN pg_catalog.pg_class c ON c.oid = a.adrelid WHERE a.oid = d.objid) \
                 WHEN 'pg_catalog.pg_trigger'::regclass THEN (SELECT c.relnamespace FROM pg_catalog.pg_trigger t \
                     JOIN pg_catalog.pg_class c ON c.oid = t.tgrelid WHERE t.oid = d.objid) \
                 WHEN 'pg_catalog.pg_policy'::regclass THEN (SELECT c.relnamespace FROM pg_catalog.pg_policy p \
                     JOIN pg_catalog.pg_class c ON c.oid = p.polrelid WHERE p.oid = d.objid) \
                 WHEN 'pg_catalog.pg_extension'::regclass THEN \
                     (SELECT extnamespace FROM pg_catalog.pg_extension WHERE oid = d.objid) \
                 WHEN 'pg_catalog.pg_statistic_ext'::regclass THEN \
                     (SELECT stxnamespace FROM pg_catalog.pg_statistic_ext WHERE oid = d.objid) \
                 WHEN 'pg_catalog.pg_opclass'::regclass THEN \
                     (SELECT opcnamespace FROM pg_catalog.pg_opclass WHERE oid = d.objid) \
                 WHEN 'pg_catalog.pg_opfamily'::regclass THEN \
                     (SELECT opfnamespace FROM pg_catalog.pg_opfamily WHERE oid = d.objid) \
                 WHEN 'pg_catalog.pg_amop'::regclass THEN (SELECT f.opfnamespace FROM pg_catalog.pg_amop o \
                     JOIN pg_catalog.pg_opfamily f ON f.oid = o.amopfamily WHERE o.oid = d.objid) \
                 WHEN 'pg_catalog.pg_amproc'::regclass THEN (SELECT f.opfnamespace FROM pg_catalog.pg_amproc p \
                     JOIN pg_catalog.pg_opfamily f ON f.oid = p.amprocfamily WHERE p.oid = d.objid) \
                 WHEN 'pg_catalog.pg_conversion'::regclass THEN \
                     (SELECT connamespace FROM pg_catalog.pg_conversion WHERE oid = d.objid) \
                 WHEN 'pg_catalog.pg_ts_config'::regclass THEN \
                     (SELECT cfgnamespace FROM pg_catalog.pg_ts_config WHERE oid = d.objid) \
                 WHEN 'pg_catalog.pg_ts_dict'::regclass THEN \
                     (SELECT dictnamespace FROM pg_catalog.pg_ts_dict WHERE oid = d.objid) \
             END AS nsp \
             FROM pg_catalog.pg_depend d \
             JOIN inside i ON i.classid = d.refclassid AND i.objid = d.refobjid \
             WHERE d.deptype IN ('n', 'a') \
             AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_depend m \
                 JOIN pg_catalog.pg_extension e ON e.oid = m.refobjid \
                 WHERE m.classid = d.classid AND m.objid = d.objid AND m.deptype = 'e' \
                 AND m.refclassid = 'pg_catalog.pg_extension'::regclass AND e.extnamespace = {ns_oid}) \
         ) \
         SELECT DISTINCT pg_catalog.pg_describe_object(classid, objid, objsubid) FROM dependents \
         WHERE nsp IS DISTINCT FROM {ns_oid} ORDER BY 1"
    )
}

async fn reset_postgres(conn: &mut Conn, namespace: &str) -> Result<(), String> {
    let dependents = conn.run(&postgres_dependents_sql(namespace), DEPENDENTS_SHOWN, None).await?;
    let names: Vec<String> = dependents.text_rows().iter().map(|row| text_at(row, 0)).collect();
    refuse_dependents(namespace, names, dependents.truncated, "Restoring would delete these.")?;
    let owner = first_text(
        &conn
            .run(
                &format!(
                    "SELECT pg_catalog.pg_get_userbyid(nspowner) FROM pg_catalog.pg_namespace WHERE nspname = {}",
                    quote_literal(namespace)
                ),
                1,
                None,
            )
            .await?,
    );
    if owner.is_empty() {
        return Err(format!("The schema “{namespace}” no longer exists."));
    }
    let dialect = dialect(Driver::Postgres);
    if let Some(sql) = dialect.drop_namespace_sql(namespace) {
        conn.execute(&sql).await?;
    }
    conn.execute(&format!("CREATE SCHEMA {} AUTHORIZATION {}", quote_double(namespace), quote_double(&owner)))
        .await?;
    /*
     * Only the schema itself, unlike `use_namespace_sql`: with public on the
     * path, the backup's `DROP ... IF EXISTS` statements could reach it.
     */
    conn.execute(&format!("SET search_path TO {}", quote_double(namespace))).await?;
    Ok(())
}

/// The dump's statements are unqualified, so they always land in the main database.
async fn reset_sqlite(conn: &mut Conn, namespace: &str) -> Result<(), String> {
    if !namespace.is_empty() && namespace != "main" {
        return Err("SQLite backups can only be restored into the main database.".into());
    }
    conn.execute("PRAGMA foreign_keys = OFF").await?;
    let entries = conn
        .run(
            "SELECT type, name, sql FROM main.sqlite_master \
             WHERE type IN ('table', 'view', 'trigger') AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\'",
            usize::MAX,
            None,
        )
        .await?
        .text_rows();
    let virtual_tables: Vec<String> = entries
        .iter()
        .filter(|row| text_at(row, 0) == "table" && text_at(row, 2).to_ascii_uppercase().starts_with("CREATE VIRTUAL TABLE"))
        .map(|row| text_at(row, 1))
        .collect();
    let shadow = |name: &str| virtual_tables.iter().any(|parent| name.starts_with(&format!("{parent}_")));
    for (kind, keyword) in [("trigger", "TRIGGER"), ("view", "VIEW"), ("table", "TABLE")] {
        for row in entries.iter().filter(|row| text_at(row, 0) == kind) {
            let name = text_at(row, 1);
            if kind == "table" && shadow(&name) {
                continue;
            }
            conn.execute(&format!("DROP {keyword} IF EXISTS main.{}", quote_double(&name))).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    fn sample() -> BackupInfo {
        BackupInfo {
            version: BACKUP_VERSION,
            driver: Driver::Postgres,
            namespace: "app".into(),
            server: "PostgreSQL 16.4".into(),
            created_at: "2026-09-27T01:50:00Z".into(),
            skipped: vec!["events_2024 (partitions aren't supported yet)".into()],
        }
    }

    #[test]
    fn round_trips_the_header() {
        let text = format!("{}-- Recon SQL export\n-- Server: other\n", header(&sample()));
        let parsed = parse_header(&mut BufReader::new(text.as_bytes())).unwrap();
        assert_eq!(parsed, sample());
    }

    #[test]
    fn reads_a_gzipped_header() {
        use flate2::bufread::MultiGzDecoder;
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        let mut bytes = Vec::new();
        for part in [header(&sample()), "-- Recon SQL export\nSELECT 1;\n".to_string()] {
            let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
            encoder.write_all(part.as_bytes()).unwrap();
            bytes.extend(encoder.finish().unwrap());
        }
        let mut reader = BufReader::new(MultiGzDecoder::new(BufReader::new(bytes.as_slice())));
        assert_eq!(parse_header(&mut reader).unwrap(), sample());
        let mut rest = String::new();
        reader.read_to_string(&mut rest).unwrap();
        assert!(rest.ends_with("SELECT 1;\n"), "{rest}");
    }

    #[test]
    fn rejects_files_that_are_not_backups() {
        let err = parse_header(&mut BufReader::new("-- Recon SQL export\n".as_bytes())).unwrap_err();
        assert_eq!(err, NOT_A_BACKUP);
        let err = parse_header(&mut BufReader::new(&b""[..])).unwrap_err();
        assert_eq!(err, NOT_A_BACKUP);
        let err = parse_header(&mut BufReader::new(&[0x1f, 0x8b, 0xff, 0x00][..])).unwrap_err();
        assert_eq!(err, NOT_A_BACKUP);
    }

    #[test]
    fn rejects_newer_versions_and_missing_drivers() {
        let newer = format!("{MARKER}{}\n-- Driver: mysql\n\n", BACKUP_VERSION + 1);
        let err = parse_header(&mut BufReader::new(newer.as_bytes())).unwrap_err();
        assert!(err.contains("newer version"), "{err}");
        let unknown = format!("{MARKER}1\n-- Driver: oracle\n\n");
        assert!(parse_header(&mut BufReader::new(unknown.as_bytes())).is_err());
    }

    #[test]
    fn keeps_header_values_on_one_line() {
        let info = BackupInfo { namespace: "a\nb".into(), ..sample() };
        let parsed = parse_header(&mut BufReader::new(header(&info).as_bytes())).unwrap();
        assert_eq!(parsed.namespace, "a b");
    }

    async fn sqlite_backup(conn: &mut Conn) -> String {
        use super::super::dump::{self, Dump, DumpProgress, DumpTable};
        use std::sync::atomic::AtomicBool;

        let tables: Vec<DumpTable> = conn
            .run(&dialect(Driver::Sqlite).tables_sql("main"), usize::MAX, None)
            .await
            .unwrap()
            .text_rows()
            .iter()
            .map(|row| DumpTable { name: text_at(row, 0), view: text_at(row, 1) == "view" })
            .collect();
        let mut out: Vec<u8> = Vec::new();
        let cancel = AtomicBool::new(false);
        let mut progress = |_: DumpProgress| {};
        let mut dump = Dump::new(&mut out, &cancel, &mut progress);
        let options = dump::DumpOptions { structure: true, data: true, drop_tables: true, routines: true };
        dump::run(conn, "SQLite 3", "main", &tables, options, &mut dump).await.unwrap();
        let info = BackupInfo { driver: Driver::Sqlite, namespace: "main".into(), skipped: Vec::new(), ..sample() };
        format!("{}{}", header(&info), String::from_utf8(out).unwrap())
    }

    async fn replay(conn: &mut Conn, sql: &str) {
        let mut splitter = crate::db::sql_split::Splitter::new(Driver::Sqlite);
        let mut statements = Vec::new();
        for line in sql.as_bytes().split_inclusive(|byte| *byte == b'\n') {
            splitter.push_bytes(line, &mut statements).unwrap();
        }
        splitter.finish(&mut statements);
        for statement in &statements {
            conn.execute(&statement.sql).await.unwrap_or_else(|err| panic!("{err}: {}", statement.sql));
        }
    }

    async fn names(conn: &mut Conn) -> Vec<String> {
        conn.run("SELECT type || ' ' || name FROM main.sqlite_master ORDER BY 1", usize::MAX, None)
            .await
            .unwrap()
            .text_rows()
            .iter()
            .map(|row| text_at(row, 0))
            .collect()
    }

    #[tokio::test]
    async fn restores_a_sqlite_backup_over_changed_data() {
        use sqlx::Connection;

        let mut conn = Conn::Sqlite(sqlx::SqliteConnection::connect("sqlite::memory:").await.unwrap());
        for sql in [
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
            "CREATE TABLE posts (id INTEGER PRIMARY KEY, user_id INT REFERENCES users(id), body TEXT)",
            "CREATE INDEX posts_user ON posts (user_id)",
            "CREATE VIEW names AS SELECT name FROM users",
            "CREATE TRIGGER stamp AFTER INSERT ON posts BEGIN UPDATE users SET name = name || '+' WHERE id = NEW.user_id; END",
            "INSERT INTO users VALUES (1, 'ann'), (2, 'bob')",
            "INSERT INTO posts VALUES (1, 1, 'hi')",
        ] {
            conn.execute(sql).await.unwrap();
        }
        let backup = sqlite_backup(&mut conn).await;
        let before = names(&mut conn).await;
        let compare = "SELECT u.id, u.name, p.body FROM users u LEFT JOIN posts p ON p.user_id = u.id ORDER BY u.id";
        let rows = conn.run(compare, 10, None).await.unwrap().text_rows();
        assert_eq!(parse_header(&mut std::io::BufReader::new(backup.as_bytes())).unwrap().driver, Driver::Sqlite);

        for sql in [
            "INSERT INTO users VALUES (3, 'cy')",
            "DELETE FROM posts",
            "CREATE TABLE stray (a INT)",
            "CREATE VIEW stray_view AS SELECT a FROM stray",
            "CREATE TRIGGER stray_trigger AFTER INSERT ON users BEGIN DELETE FROM stray; END",
        ] {
            conn.execute(sql).await.unwrap();
        }
        reset_namespace(&mut conn, Driver::Sqlite, "main").await.unwrap();
        assert!(names(&mut conn).await.is_empty());
        replay(&mut conn, &backup).await;

        assert_eq!(names(&mut conn).await, before);
        assert_eq!(conn.run(compare, 10, None).await.unwrap().text_rows(), rows);
        conn.execute("INSERT INTO posts VALUES (2, 2, 'again')").await.unwrap();
        let bob = conn.run("SELECT name FROM users WHERE id = 2", 1, None).await.unwrap();
        assert_eq!(first_text(&bob), "bob+");

        reset_namespace(&mut conn, Driver::Sqlite, "main").await.unwrap();
        let empty = sqlite_backup(&mut conn).await;
        replay(&mut conn, &empty).await;
        assert!(names(&mut conn).await.is_empty());
        conn.close().await;
    }

    #[tokio::test]
    async fn only_restores_sqlite_into_main() {
        use sqlx::Connection;

        let mut conn = Conn::Sqlite(sqlx::SqliteConnection::connect("sqlite::memory:").await.unwrap());
        let err = reset_namespace(&mut conn, Driver::Sqlite, "other").await.unwrap_err();
        assert!(err.contains("main database"), "{err}");
        conn.close().await;
    }

    async fn open_live(driver: &str, port: u16, user: &str, database: &str) -> crate::db::Opened {
        let entry: crate::models::ConnectionEntry = serde_json::from_value(serde_json::json!({
            "id": "live", "name": "live", "driver": driver, "host": "127.0.0.1", "port": port,
            "user": user, "database": database, "filePath": "", "sslMode": "prefer", "headerColor": "",
            "savePassword": false,
        }))
        .unwrap();
        match driver {
            "postgres" => super::super::postgres::open(&entry, None, &Default::default()).await.unwrap(),
            _ => super::super::mysql::open(
                &entry,
                std::env::var("RECON_TEST_MYSQL_PASSWORD").ok().as_deref(),
                &Default::default(),
            )
            .await
            .unwrap(),
        }
    }

    /// Resets inside a transaction that's always rolled back, returning the refusal if there was one.
    async fn try_reset(conn: &mut Conn, driver: Driver, namespace: &str) -> Result<(), String> {
        if driver == Driver::Postgres {
            conn.execute("BEGIN").await.unwrap();
        }
        let reset = reset_namespace(conn, driver, namespace).await;
        if driver == Driver::Postgres {
            conn.execute("ROLLBACK").await.unwrap();
        }
        reset
    }

    /**
     * Checks the Postgres dependency check against objects that live in the
     * schema, which must not block a restore, and each kind of object in
     * another schema that must: `RECON_TEST_POSTGRES_PORT=5432 cargo test -- --ignored`.
     */
    #[tokio::test]
    #[ignore]
    async fn live_postgres_dependency_check() {
        let Some(port) = std::env::var("RECON_TEST_POSTGRES_PORT").ok().and_then(|port| port.parse().ok()) else {
            return;
        };
        let admin = open_live("postgres", port, "postgres", "postgres").await;
        let mut admin_conn = admin.conn;
        admin_conn.execute("DROP DATABASE IF EXISTS recon_restore_probe").await.unwrap();
        admin_conn.execute("CREATE DATABASE recon_restore_probe").await.unwrap();
        let probe = open_live("postgres", port, "postgres", "recon_restore_probe").await;
        let mut conn = probe.conn;
        for sql in [
            "CREATE SCHEMA app",
            "CREATE SCHEMA other",
            "SET search_path TO app",
            "CREATE EXTENSION citext SCHEMA app",
            "CREATE TABLE t (id int PRIMARY KEY, a int, b int, e citext)",
            "CREATE STATISTICS t_stats (dependencies) ON a, b FROM t",
            "CREATE COLLATION ci (provider = icu, locale = 'und-u-ks-level2', deterministic = false)",
            "CREATE TABLE u (id int, name text COLLATE ci)",
            "CREATE FUNCTION int_eq(int, int) RETURNS bool LANGUAGE sql IMMUTABLE AS 'SELECT $1 = $2'",
            "CREATE OPERATOR === (LEFTARG = int, RIGHTARG = int, FUNCTION = int_eq)",
            "ALTER TABLE t ENABLE ROW LEVEL SECURITY",
            "CREATE POLICY p ON t USING (a > 0)",
            "ALTER DEFAULT PRIVILEGES IN SCHEMA app GRANT SELECT ON TABLES TO PUBLIC",
            "CREATE TYPE mood AS ENUM ('a', 'b')",
            "CREATE MATERIALIZED VIEW mv AS SELECT id FROM t",
            "CREATE INDEX ON mv (id)",
            "CREATE TABLE child (id int PRIMARY KEY, t_id int REFERENCES t (id), feeling mood DEFAULT 'a')",
            "CREATE FUNCTION touch() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$",
            "CREATE TRIGGER touch BEFORE INSERT ON child FOR EACH ROW EXECUTE FUNCTION touch()",
            "CREATE VIEW v AS SELECT id FROM t",
            "SET search_path TO public",
        ] {
            conn.execute(sql).await.unwrap_or_else(|err| panic!("{err}: {sql}"));
        }
        try_reset(&mut conn, Driver::Postgres, "app").await.unwrap();

        for (setup, expected) in [
            ("CREATE VIEW other.peek AS SELECT id FROM app.t", "view other.peek"),
            ("CREATE TABLE other.feelings (f app.mood)", "other.feelings"),
            ("CREATE TABLE other.emails (e app.citext)", "other.emails"),
            ("CREATE TABLE other.refs (t_id int REFERENCES app.t (id))", "other.refs"),
            ("CREATE FUNCTION other.lookup(app.t) RETURNS int LANGUAGE sql AS 'SELECT 1'", "other.lookup"),
        ] {
            conn.execute(setup).await.unwrap_or_else(|err| panic!("{err}: {setup}"));
            let err = try_reset(&mut conn, Driver::Postgres, "app").await.unwrap_err();
            assert!(err.contains(expected), "{setup}: {err}");
            conn.execute("DROP SCHEMA other CASCADE").await.unwrap();
            conn.execute("CREATE SCHEMA other").await.unwrap();
        }
        try_reset(&mut conn, Driver::Postgres, "app").await.unwrap();

        conn.close().await;
        probe.pool.close().await;
        admin_conn.execute("DROP DATABASE recon_restore_probe").await.unwrap();
        admin_conn.close().await;
        admin.pool.close().await;
    }

    /**
     * Checks that views and foreign keys in another database block a restore:
     * `RECON_TEST_MYSQL_USER=root cargo test -- --ignored`.
     */
    #[tokio::test]
    #[ignore]
    async fn live_mysql_dependency_check() {
        let Ok(user) = std::env::var("RECON_TEST_MYSQL_USER") else {
            return;
        };
        let opened = open_live("mysql", 3306, &user, "").await;
        let mut conn = opened.conn;
        for sql in [
            "DROP DATABASE IF EXISTS recon_restore_a",
            "DROP DATABASE IF EXISTS recon_restore_b",
            "CREATE DATABASE recon_restore_a",
            "CREATE DATABASE recon_restore_b",
            "CREATE TABLE recon_restore_a.t (id INT PRIMARY KEY)",
            "CREATE FUNCTION recon_restore_a.twice(x INT) RETURNS INT DETERMINISTIC RETURN x * 2",
            "CREATE VIEW recon_restore_a.v AS SELECT id FROM recon_restore_a.t",
        ] {
            conn.execute(sql).await.unwrap_or_else(|err| panic!("{err}: {sql}"));
        }
        for (setup, expected) in [
            ("CREATE VIEW recon_restore_b.peek AS SELECT id FROM recon_restore_a.t", "view recon_restore_b.peek"),
            ("CREATE VIEW recon_restore_b.calls AS SELECT recon_restore_a.twice(1) AS x", "view recon_restore_b.calls"),
            (
                "CREATE TABLE recon_restore_b.refs (t_id INT, CONSTRAINT refs_t FOREIGN KEY (t_id) REFERENCES recon_restore_a.t (id))",
                "foreign key refs_t on recon_restore_b.refs",
            ),
        ] {
            conn.execute(setup).await.unwrap_or_else(|err| panic!("{err}: {setup}"));
            let err = try_reset(&mut conn, Driver::Mysql, "recon_restore_a").await.unwrap_err();
            assert!(err.contains(expected), "{setup}: {err}");
            conn.execute("DROP DATABASE recon_restore_b").await.unwrap();
            conn.execute("CREATE DATABASE recon_restore_b").await.unwrap();
        }
        let tables = "SELECT COUNT(*) FROM information_schema.TABLES WHERE TABLE_SCHEMA = 'recon_restore_a'";
        assert_eq!(first_text(&conn.run(tables, 1, None).await.unwrap()), "2");
        try_reset(&mut conn, Driver::Mysql, "recon_restore_a").await.unwrap();
        assert_eq!(first_text(&conn.run(tables, 1, None).await.unwrap()), "0");

        conn.execute("DROP DATABASE recon_restore_a").await.unwrap();
        conn.execute("DROP DATABASE recon_restore_b").await.unwrap();
        conn.close().await;
        opened.pool.close().await;
    }

    #[test]
    fn formats_utc_timestamps() {
        let stamp = utc_timestamp();
        assert_eq!(stamp.len(), 20, "{stamp}");
        assert!(stamp.ends_with('Z') && stamp.as_bytes()[10] == b'T', "{stamp}");
    }
}
