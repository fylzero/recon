use serde::Deserialize;

use super::{dialect, quote_double, quote_literal};
use crate::models::Driver;

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TruncateOptions {
    /// MySQL and SQLite only. Postgres can't skip the check, so it uses `cascade` instead.
    #[serde(default)]
    pub disable_foreign_keys: bool,
    /// Postgres only: also empties every table with a foreign key to one of these.
    #[serde(default)]
    pub cascade: bool,
    /// Postgres and SQLite only. MySQL's TRUNCATE always resets AUTO_INCREMENT.
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
    /// MySQL and SQLite only. Postgres can't skip the check, so it uses `cascade` instead.
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
    let qualified: Vec<String> = tables.iter().map(|table| dialect.qualified(namespace, table)).collect();
    let mut sql = Vec::new();
    match driver {
        Driver::Mysql => {
            if options.disable_foreign_keys {
                sql.push("SET FOREIGN_KEY_CHECKS = 0".to_string());
            }
            sql.extend(qualified.iter().map(|table| format!("TRUNCATE TABLE {table}")));
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
    }
    sql
}

/** Like `truncate_statements`, for removing `tables` altogether. */
pub fn drop_statements(driver: Driver, namespace: &str, tables: &[String], options: DropOptions) -> Vec<String> {
    let dialect = dialect(driver);
    let qualified: Vec<String> = tables.iter().map(|table| dialect.qualified(namespace, table)).collect();
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
        let options = TruncateOptions { disable_foreign_keys: true, ..Default::default() };
        assert_eq!(
            truncate_statements(Driver::Mysql, "app", &names(&["users", "posts"]), options, false),
            ["SET FOREIGN_KEY_CHECKS = 0", "TRUNCATE TABLE `app`.`users`", "TRUNCATE TABLE `app`.`posts`"]
        );
        assert_eq!(
            truncate_statements(Driver::Mysql, "app", &names(&["users"]), TruncateOptions::default(), false),
            ["TRUNCATE TABLE `app`.`users`"]
        );
    }

    #[test]
    fn builds_one_postgres_truncate() {
        let options = TruncateOptions { restart_identity: true, cascade: true, ..Default::default() };
        assert_eq!(
            truncate_statements(Driver::Postgres, "public", &names(&["users", "posts"]), options, false),
            ["TRUNCATE TABLE \"public\".\"users\", \"public\".\"posts\" RESTART IDENTITY CASCADE"]
        );
        assert_eq!(
            truncate_statements(Driver::Postgres, "public", &names(&["users"]), TruncateOptions::default(), false),
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
        conn.run(sql, 1, None).await.unwrap().rows.remove(0).remove(0)
    }

    #[tokio::test]
    async fn empties_sqlite_tables_and_restarts_their_counters() {
        let mut conn = Conn::Sqlite(sqlx::SqliteConnection::connect("sqlite::memory:").await.unwrap());
        for sql in [
            "CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT)",
            "CREATE TABLE posts (id INTEGER PRIMARY KEY, user_id INTEGER REFERENCES users (id))",
            "INSERT INTO users (name) VALUES ('ada'), ('bob')",
            "INSERT INTO posts (user_id) VALUES (1)",
        ] {
            conn.run(sql, 0, None).await.unwrap();
        }
        let users = names(&["users"]);

        let checked = truncate_statements(Driver::Sqlite, "main", &users, TruncateOptions::default(), true);
        let error = run_all(&mut conn, &checked).await.unwrap_err();
        assert!(error.contains("FOREIGN KEY"), "{error}");
        conn.run("ROLLBACK", 0, None).await.unwrap();
        assert_eq!(scalar(&mut conn, "SELECT COUNT(*) FROM users").await, CellValue::Int(2));

        let has_sequence = scalar(&mut conn, &sqlite_sequence_sql("main")).await == CellValue::Int(1);
        assert!(has_sequence);
        let options = TruncateOptions { disable_foreign_keys: true, restart_identity: true, ..Default::default() };
        run_all(&mut conn, &truncate_statements(Driver::Sqlite, "main", &users, options, has_sequence)).await.unwrap();
        assert_eq!(scalar(&mut conn, "SELECT COUNT(*) FROM users").await, CellValue::Int(0));
        assert_eq!(scalar(&mut conn, "SELECT COUNT(*) FROM posts").await, CellValue::Int(1));
        conn.run("INSERT INTO users (name) VALUES ('cy')", 0, None).await.unwrap();
        assert_eq!(scalar(&mut conn, "SELECT id FROM users").await, CellValue::Int(1));
    }

    #[test]
    fn builds_one_drop_for_mysql_and_postgres() {
        let tables = names(&["users", "posts"]);
        let options = DropOptions { disable_foreign_keys: true, cascade: true };
        assert_eq!(
            drop_statements(Driver::Mysql, "app", &tables, options),
            ["SET FOREIGN_KEY_CHECKS = 0", "DROP TABLE `app`.`users`, `app`.`posts`"]
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
        let mut conn = Conn::Sqlite(sqlx::SqliteConnection::connect("sqlite::memory:").await.unwrap());
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

        let options = DropOptions { disable_foreign_keys: true, ..Default::default() };
        run_all(&mut conn, &drop_statements(Driver::Sqlite, "main", &users, options)).await.unwrap();
        assert_eq!(scalar(&mut conn, tables).await, CellValue::Int(1));
        assert_eq!(scalar(&mut conn, "SELECT COUNT(*) FROM posts").await, CellValue::Int(1));
    }
}
