use serde::Deserialize;

use super::{dialect, mssql, quote_double, quote_literal};
use crate::models::Driver;

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TruncateOptions {
    /// Not Postgres, which can't skip the check, so it uses `cascade` instead.
    #[serde(default)]
    pub disable_foreign_keys: bool,
    /// Postgres only: also empties every table with a foreign key to one of these.
    #[serde(default)]
    pub cascade: bool,
    /// Not MySQL, whose TRUNCATE always resets AUTO_INCREMENT.
    #[serde(default)]
    pub restart_identity: bool,
}

/// Whether the SQLite database has a `sqlite_sequence` table, which only exists once a table uses AUTOINCREMENT.
pub fn sqlite_sequence_sql(namespace: &str) -> String {
    format!(
        "SELECT COUNT(*) FROM {}.sqlite_master WHERE type = 'table' AND name = 'sqlite_sequence'",
        quote_double(namespace)
    )
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DropOptions {
    /// Not Postgres, which can't skip the check, so it uses `cascade` instead.
    #[serde(default)]
    pub disable_foreign_keys: bool,
    /// Postgres only: also drops the views and foreign key constraints that depend on these tables.
    #[serde(default)]
    pub cascade: bool,
}

/**
 * The statements that empty `tables`, to run in order on a connection of
 * their own so a disabled foreign key check can't leak into the pool.
 * SQLite has no TRUNCATE, so it deletes every row inside a transaction and
 * clears the AUTOINCREMENT counters when `has_sequence` says there are any.
 */
pub fn truncate_statements(
    driver: Driver,
    namespace: &str,
    tables: &[String],
    options: TruncateOptions,
    has_sequence: bool,
) -> Vec<String> {
    let dialect = dialect(driver);
    let qualified: Vec<String> = tables
        .iter()
        .map(|table| dialect.qualified(namespace, table))
        .collect();
    let mut sql = Vec::new();
    match driver {
        Driver::Mysql => {
            if options.disable_foreign_keys {
                sql.push("SET FOREIGN_KEY_CHECKS = 0".to_string());
            }
            sql.extend(
                qualified
                    .iter()
                    .map(|table| format!("TRUNCATE TABLE {table}")),
            );
        }
        Driver::Postgres => {
            let mut statement = format!("TRUNCATE TABLE {}", qualified.join(", "));
            if options.restart_identity {
                statement.push_str(" RESTART IDENTITY");
            }
            if options.cascade {
                statement.push_str(" CASCADE");
            }
            sql.push(statement);
        }
        Driver::Sqlite => {
            if options.disable_foreign_keys {
                sql.push("PRAGMA foreign_keys = OFF".to_string());
            }
            sql.push("BEGIN".to_string());
            sql.extend(qualified.iter().map(|table| format!("DELETE FROM {table}")));
            if options.restart_identity && has_sequence {
                let names: Vec<String> = tables.iter().map(|table| quote_literal(table)).collect();
                sql.push(format!(
                    "DELETE FROM {}.sqlite_sequence WHERE name IN ({})",
                    quote_double(namespace),
                    names.join(", ")
                ));
            }
            sql.push("COMMIT".to_string());
        }
        Driver::Mssql => {
            sql.extend(
                dialect
                    .use_namespace_sql(namespace)
                    .filter(|_| !namespace.is_empty()),
            );
            sql.push(mssql_truncate(&qualified, options));
        }
    }
    sql
}

/**
 * Collects the foreign keys that point at one of `qualified` into
 * `@keys (tbl, name)`. `between_targets` keeps only keys whose own table is
 * one of them too, and `enabled` only keys that are switched on.
 */
fn mssql_keys_to(qualified: &[String], between_targets: bool, enabled: bool) -> String {
    let targets = qualified
        .iter()
        .map(|table| format!("OBJECT_ID({})", mssql::n_literal(table)))
        .collect::<Vec<_>>()
        .join(", ");
    let mut filter = format!("fk.referenced_object_id IN ({targets})");
    if between_targets {
        filter.push_str(&format!(" AND fk.parent_object_id IN ({targets})"));
    }
    if enabled {
        filter.push_str(" AND fk.is_disabled = 0");
    }
    format!(
        "DECLARE @keys TABLE (tbl nvarchar(600), name sysname);\n\
         INSERT INTO @keys SELECT QUOTENAME(s.name) + N'.' + QUOTENAME(t.name), fk.name \
         FROM sys.foreign_keys fk \
         JOIN sys.tables t ON t.object_id = fk.parent_object_id \
         JOIN sys.schemas s ON s.schema_id = t.schema_id \
         WHERE {filter};\n\
         DECLARE @sql nvarchar(max);\n"
    )
}

/// Runs `ALTER TABLE <tbl> <action> <name>` for every key collected by `mssql_keys_to`.
fn mssql_each_key(action: &str) -> String {
    format!(
        "SET @sql = N'';\n\
         SELECT @sql += N'ALTER TABLE ' + tbl + N' {action} ' + QUOTENAME(name) + N';' FROM @keys;\n\
         EXEC sp_executesql @sql;\n"
    )
}

/**
 * TRUNCATE also resets the identity and is refused while any foreign key
 * points at the table, so it's only used when both are wanted anyway.
 * Disabled keys are turned back on without rechecking, so rows that point
 * here stay. One batch with XACT_ABORT, so a failure changes nothing.
 */
fn mssql_truncate(qualified: &[String], options: TruncateOptions) -> String {
    let mut sql = String::from("SET XACT_ABORT ON;\nBEGIN TRAN;\n");
    if options.disable_foreign_keys {
        sql.push_str(&mssql_keys_to(qualified, false, true));
        sql.push_str(&mssql_each_key("NOCHECK CONSTRAINT"));
    }
    if options.restart_identity {
        sql.push_str("DECLARE @seed bigint;\n");
    }
    for table in qualified {
        let name = mssql::n_literal(table);
        if !options.restart_identity {
            sql.push_str(&format!("DELETE FROM {table};\n"));
            continue;
        }
        /*
         * RESEED sets the last value used, except on a table that has never
         * had a row, where it's the next one. Skipping those keeps them at
         * their seed.
         */
        sql.push_str(&format!(
            "IF EXISTS (SELECT 1 FROM sys.foreign_keys WHERE referenced_object_id = OBJECT_ID({name}))\n\
             BEGIN\n\
             DELETE FROM {table};\n\
             SET @seed = (SELECT CAST(seed_value AS bigint) - CAST(increment_value AS bigint) \
             FROM sys.identity_columns WHERE object_id = OBJECT_ID({name}) AND last_value IS NOT NULL);\n\
             IF @seed IS NOT NULL DBCC CHECKIDENT ({name}, RESEED, @seed);\n\
             END\n\
             ELSE TRUNCATE TABLE {table};\n"
        ));
    }
    if options.disable_foreign_keys {
        sql.push_str(&mssql_each_key("CHECK CONSTRAINT"));
    }
    sql.push_str("COMMIT;");
    sql
}

/**
 * Foreign keys between the dropped tables go first, since DROP TABLE won't
 * drop a table another one still references. With `disable_foreign_keys`,
 * keys in other tables that point here are dropped too: SQL Server can't
 * leave them pointing at a missing table.
 */
fn mssql_drop(qualified: &[String], options: DropOptions) -> String {
    let mut sql = String::from("SET XACT_ABORT ON;\nBEGIN TRAN;\n");
    sql.push_str(&mssql_keys_to(
        qualified,
        !options.disable_foreign_keys,
        false,
    ));
    sql.push_str(&mssql_each_key("DROP CONSTRAINT"));
    sql.push_str(&format!(
        "DROP TABLE IF EXISTS {};\nCOMMIT;",
        qualified.join(", ")
    ));
    sql
}

/**
 * A new table with an auto-incrementing `id` primary key, since MySQL and
 * SQLite can't create a table without columns.
 */
pub fn create_statement(driver: Driver, namespace: &str, table: &str) -> String {
    let qualified = dialect(driver).qualified(namespace, table);
    let id = match driver {
        Driver::Mysql => "`id` bigint unsigned NOT NULL AUTO_INCREMENT PRIMARY KEY",
        Driver::Postgres => "\"id\" bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY",
        Driver::Sqlite => "\"id\" INTEGER PRIMARY KEY AUTOINCREMENT",
        Driver::Mssql => "[id] bigint IDENTITY(1,1) NOT NULL PRIMARY KEY",
    };
    format!("CREATE TABLE {qualified} ({id})")
}

/** Like `truncate_statements`, for removing `tables` altogether. */
pub fn drop_statements(
    driver: Driver,
    namespace: &str,
    tables: &[String],
    options: DropOptions,
) -> Vec<String> {
    let dialect = dialect(driver);
    let qualified: Vec<String> = tables
        .iter()
        .map(|table| dialect.qualified(namespace, table))
        .collect();
    let mut sql = Vec::new();
    match driver {
        Driver::Mysql => {
            if options.disable_foreign_keys {
                sql.push("SET FOREIGN_KEY_CHECKS = 0".to_string());
            }
            sql.push(format!("DROP TABLE {}", qualified.join(", ")));
        }
        Driver::Postgres => {
            let mut statement = format!("DROP TABLE {}", qualified.join(", "));
            if options.cascade {
                statement.push_str(" CASCADE");
            }
            sql.push(statement);
        }
        Driver::Sqlite => {
            if options.disable_foreign_keys {
                sql.push("PRAGMA foreign_keys = OFF".to_string());
            }
            sql.push("BEGIN".to_string());
            sql.extend(qualified.iter().map(|table| format!("DROP TABLE {table}")));
            sql.push("COMMIT".to_string());
        }
        Driver::Mssql => {
            sql.extend(
                dialect
                    .use_namespace_sql(namespace)
                    .filter(|_| !namespace.is_empty()),
            );
            sql.push(mssql_drop(&qualified, options));
        }
    }
    sql
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{CellValue, Conn};
    use sqlx::Connection;

    fn names(tables: &[&str]) -> Vec<String> {
        tables.iter().map(|table| table.to_string()).collect()
    }

    #[test]
    fn builds_mysql_truncates() {
        let options = TruncateOptions {
            disable_foreign_keys: true,
            ..Default::default()
        };
        assert_eq!(
            truncate_statements(
                Driver::Mysql,
                "app",
                &names(&["users", "posts"]),
                options,
                false
            ),
            [
                "SET FOREIGN_KEY_CHECKS = 0",
                "TRUNCATE TABLE `app`.`users`",
                "TRUNCATE TABLE `app`.`posts`"
            ]
        );
        assert_eq!(
            truncate_statements(
                Driver::Mysql,
                "app",
                &names(&["users"]),
                TruncateOptions::default(),
                false
            ),
            ["TRUNCATE TABLE `app`.`users`"]
        );
    }

    #[test]
    fn builds_one_postgres_truncate() {
        let options = TruncateOptions {
            restart_identity: true,
            cascade: true,
            ..Default::default()
        };
        assert_eq!(
            truncate_statements(Driver::Postgres, "public", &names(&["users", "posts"]), options, false),
            ["TRUNCATE TABLE \"public\".\"users\", \"public\".\"posts\" RESTART IDENTITY CASCADE"]
        );
        assert_eq!(
            truncate_statements(
                Driver::Postgres,
                "public",
                &names(&["users"]),
                TruncateOptions::default(),
                false
            ),
            ["TRUNCATE TABLE \"public\".\"users\""]
        );
    }

    async fn run_all(conn: &mut Conn, sql: &[String]) -> Result<(), String> {
        for statement in sql {
            conn.run(statement, 0, None).await?;
        }
        Ok(())
    }

    async fn scalar(conn: &mut Conn, sql: &str) -> CellValue {
        conn.run(sql, 1, None)
            .await
            .unwrap()
            .rows
            .remove(0)
            .remove(0)
    }

    #[tokio::test]
    async fn empties_sqlite_tables_and_restarts_their_counters() {
        let mut conn = Conn::Sqlite(
            sqlx::SqliteConnection::connect("sqlite::memory:")
                .await
                .unwrap(),
        );
        for sql in [
            "CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT)",
            "CREATE TABLE posts (id INTEGER PRIMARY KEY, user_id INTEGER REFERENCES users (id))",
            "INSERT INTO users (name) VALUES ('ada'), ('bob')",
            "INSERT INTO posts (user_id) VALUES (1)",
        ] {
            conn.run(sql, 0, None).await.unwrap();
        }
        let users = names(&["users"]);

        let checked = truncate_statements(
            Driver::Sqlite,
            "main",
            &users,
            TruncateOptions::default(),
            true,
        );
        let error = run_all(&mut conn, &checked).await.unwrap_err();
        assert!(error.contains("FOREIGN KEY"), "{error}");
        conn.run("ROLLBACK", 0, None).await.unwrap();
        assert_eq!(
            scalar(&mut conn, "SELECT COUNT(*) FROM users").await,
            CellValue::Int(2)
        );

        let has_sequence =
            scalar(&mut conn, &sqlite_sequence_sql("main")).await == CellValue::Int(1);
        assert!(has_sequence);
        let options = TruncateOptions {
            disable_foreign_keys: true,
            restart_identity: true,
            ..Default::default()
        };
        run_all(
            &mut conn,
            &truncate_statements(Driver::Sqlite, "main", &users, options, has_sequence),
        )
        .await
        .unwrap();
        assert_eq!(
            scalar(&mut conn, "SELECT COUNT(*) FROM users").await,
            CellValue::Int(0)
        );
        assert_eq!(
            scalar(&mut conn, "SELECT COUNT(*) FROM posts").await,
            CellValue::Int(1)
        );
        conn.run("INSERT INTO users (name) VALUES ('cy')", 0, None)
            .await
            .unwrap();
        assert_eq!(
            scalar(&mut conn, "SELECT id FROM users").await,
            CellValue::Int(1)
        );
    }

    #[test]
    fn builds_mysql_and_postgres_creates_with_an_id_key() {
        assert_eq!(
            create_statement(Driver::Mysql, "app", "posts"),
            "CREATE TABLE `app`.`posts` (`id` bigint unsigned NOT NULL AUTO_INCREMENT PRIMARY KEY)"
        );
        assert_eq!(
            create_statement(Driver::Postgres, "public", "posts"),
            "CREATE TABLE \"public\".\"posts\" (\"id\" bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY)"
        );
    }

    #[tokio::test]
    async fn creates_a_sqlite_table_that_numbers_its_rows() {
        let mut conn = Conn::Sqlite(
            sqlx::SqliteConnection::connect("sqlite::memory:")
                .await
                .unwrap(),
        );
        conn.run(&create_statement(Driver::Sqlite, "main", "posts"), 0, None)
            .await
            .unwrap();
        conn.run("INSERT INTO posts DEFAULT VALUES", 0, None)
            .await
            .unwrap();
        assert_eq!(
            scalar(&mut conn, "SELECT id FROM posts").await,
            CellValue::Int(1)
        );
    }

    #[test]
    fn builds_one_drop_for_mysql_and_postgres() {
        let tables = names(&["users", "posts"]);
        let options = DropOptions {
            disable_foreign_keys: true,
            cascade: true,
        };
        assert_eq!(
            drop_statements(Driver::Mysql, "app", &tables, options),
            [
                "SET FOREIGN_KEY_CHECKS = 0",
                "DROP TABLE `app`.`users`, `app`.`posts`"
            ]
        );
        assert_eq!(
            drop_statements(Driver::Postgres, "public", &tables, options),
            ["DROP TABLE \"public\".\"users\", \"public\".\"posts\" CASCADE"]
        );
        assert_eq!(
            drop_statements(Driver::Postgres, "public", &tables, DropOptions::default()),
            ["DROP TABLE \"public\".\"users\", \"public\".\"posts\""]
        );
    }

    #[tokio::test]
    async fn drops_referenced_sqlite_tables_only_without_foreign_key_checks() {
        let mut conn = Conn::Sqlite(
            sqlx::SqliteConnection::connect("sqlite::memory:")
                .await
                .unwrap(),
        );
        for sql in [
            "CREATE TABLE users (id INTEGER PRIMARY KEY)",
            "CREATE TABLE posts (id INTEGER PRIMARY KEY, user_id INTEGER REFERENCES users (id))",
            "INSERT INTO users VALUES (1)",
            "INSERT INTO posts (user_id) VALUES (1)",
        ] {
            conn.run(sql, 0, None).await.unwrap();
        }
        let users = names(&["users"]);
        let tables = "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'";

        let checked = drop_statements(Driver::Sqlite, "main", &users, DropOptions::default());
        let error = run_all(&mut conn, &checked).await.unwrap_err();
        assert!(error.contains("FOREIGN KEY"), "{error}");
        conn.run("ROLLBACK", 0, None).await.unwrap();
        assert_eq!(scalar(&mut conn, tables).await, CellValue::Int(2));

        let options = DropOptions {
            disable_foreign_keys: true,
            ..Default::default()
        };
        run_all(
            &mut conn,
            &drop_statements(Driver::Sqlite, "main", &users, options),
        )
        .await
        .unwrap();
        assert_eq!(scalar(&mut conn, tables).await, CellValue::Int(1));
        assert_eq!(
            scalar(&mut conn, "SELECT COUNT(*) FROM posts").await,
            CellValue::Int(1)
        );
    }

    #[test]
    fn empties_and_drops_sql_server_tables_in_one_transaction() {
        let keep = TruncateOptions::default();
        let sql = truncate_statements(Driver::Mssql, "app", &["dbo.posts".into()], keep, false);
        assert_eq!(sql[0], "USE [app]");
        assert!(sql[1].starts_with("SET XACT_ABORT ON;\nBEGIN TRAN;"));
        assert!(sql[1].contains("DELETE FROM [app].[dbo].[posts];"));
        assert!(!sql[1].contains("TRUNCATE"));
        assert!(sql[1].ends_with("COMMIT;"));

        let restart = TruncateOptions {
            disable_foreign_keys: true,
            restart_identity: true,
            ..TruncateOptions::default()
        };
        let sql = truncate_statements(
            Driver::Mssql,
            "app",
            &["dbo.o'brien".into()],
            restart,
            false,
        );
        assert!(sql[1].contains("OBJECT_ID(N'[app].[dbo].[o''brien]')"));
        assert!(sql[1].contains("ELSE TRUNCATE TABLE [app].[dbo].[o'brien];"));
        let off = sql[1].find("NOCHECK CONSTRAINT").unwrap();
        let on = sql[1].rfind("' CHECK CONSTRAINT").unwrap();
        assert!(off < sql[1].find("DELETE FROM").unwrap() && on > off);
        assert!(!sql[1].contains("WITH CHECK"));

        let drop = drop_statements(
            Driver::Mssql,
            "app",
            &["dbo.a".into(), "dbo.b".into()],
            DropOptions::default(),
        );
        assert!(drop[1].contains("AND fk.parent_object_id IN ("));
        assert!(drop[1].contains("DROP CONSTRAINT"));
        assert!(drop[1].contains("DROP TABLE IF EXISTS [app].[dbo].[a], [app].[dbo].[b];"));
        let every = DropOptions {
            disable_foreign_keys: true,
            ..DropOptions::default()
        };
        let drop = drop_statements(Driver::Mssql, "app", &["dbo.a".into()], every);
        assert!(!drop[1].contains("fk.parent_object_id IN ("));
    }
}
