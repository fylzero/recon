use serde::{Deserialize, Serialize};

use super::filter::{FilterKind, FilterNode, Fragment};
use super::{dialect, EditValue};
use crate::models::Driver;

pub const SAMPLE_LIMIT: usize = 5;
const MAX_TEXT_LENGTH: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReplaceMode {
    /// Replaces every occurrence of the text inside a value.
    Contains,
    /// Replaces values that are exactly the text.
    Whole,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplaceRequest {
    pub namespace: String,
    pub table: String,
    pub column: String,
    pub find: String,
    pub replace: String,
    pub mode: ReplaceMode,
    #[serde(default)]
    pub filter: Option<FilterNode>,
    /// Minutes east of UTC, applied to date bounds in the filter.
    #[serde(default)]
    pub utc_offset: Option<i32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplacePreview {
    pub count: u64,
    pub samples: Vec<ReplaceSample>,
    /// The UPDATE with its values written out, for showing or opening in a SQL tab.
    pub sql: String,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct ReplaceSample {
    pub before: String,
    pub after: String,
}

pub struct ReplaceStatements {
    pub count: Fragment,
    pub sample: Fragment,
    pub update: Fragment,
}

/** Enums can only take whole values, since replacing part of one would rarely leave a valid value. */
pub fn replaceable(kind: FilterKind, mode: ReplaceMode) -> bool {
    match kind {
        FilterKind::Text => true,
        FilterKind::Enum => mode == ReplaceMode::Whole,
        _ => false,
    }
}

pub fn check(request: &ReplaceRequest, kind: FilterKind) -> Result<(), String> {
    let column = &request.column;
    if !replaceable(kind, request.mode) {
        return Err(match kind {
            FilterKind::Enum => format!("{column} is an enum, so only whole values can be replaced."),
            _ => format!("Find and replace only works on text columns, and {column} isn't one."),
        });
    }
    if request.find.is_empty() {
        return Err("Enter the text to find.".into());
    }
    if request.find.len() > MAX_TEXT_LENGTH || request.replace.len() > MAX_TEXT_LENGTH {
        return Err(format!("Find and replace text can be at most {MAX_TEXT_LENGTH} characters."));
    }
    if request.find == request.replace {
        return Err("The replacement is the same as the text to find, so nothing would change.".into());
    }
    Ok(())
}

/** The value a row ends up with, worked out the same way the database's REPLACE does. */
pub fn replaced(request: &ReplaceRequest, before: &str) -> String {
    match request.mode {
        ReplaceMode::Contains => before.replace(&request.find, &request.replace),
        ReplaceMode::Whole => request.replace.clone(),
    }
}

fn text(value: &str) -> EditValue {
    EditValue::Text(value.to_string())
}

/// Postgres casts so citext compares case-sensitively and enums compare as text.
fn column_text(driver: Driver, ident: &str) -> String {
    match driver {
        Driver::Postgres => format!("{ident}::text"),
        _ => ident.to_string(),
    }
}

/** `REPLACE(column, find, replace)`, which is case-sensitive on every supported database. */
fn replace_expr(driver: Driver, ident: &str, request: &ReplaceRequest) -> Fragment {
    let mut sql = Fragment::new(format!("REPLACE({}, ", column_text(driver, ident)));
    sql.push_value(text(&request.find));
    sql.push_sql(", ");
    sql.push_value(text(&request.replace));
    sql.push_sql(")");
    sql
}

/**
 * Picks the rows whose value changes, matching case and trailing spaces
 * exactly. MySQL compares with the column's collation, which usually ignores
 * both, so its checks go through REPLACE, CHAR_LENGTH, and binary casts instead.
 */
fn match_condition(driver: Driver, ident: &str, request: &ReplaceRequest) -> Fragment {
    match (request.mode, driver) {
        (ReplaceMode::Contains, Driver::Mysql) => {
            let mut sql = Fragment::new("CAST(");
            sql.append(replace_expr(driver, ident, request));
            sql.push_sql(format!(" AS BINARY) <> CAST({ident} AS BINARY)"));
            sql
        }
        (ReplaceMode::Contains, _) => {
            let mut sql = replace_expr(driver, ident, request);
            sql.push_sql(match driver {
                Driver::Sqlite => format!(" <> {ident} COLLATE BINARY"),
                _ => format!(" <> {}", column_text(driver, ident)),
            });
            sql
        }
        (ReplaceMode::Whole, Driver::Mysql) => {
            let mut sql = Fragment::new(format!("{ident} = "));
            sql.push_value(text(&request.find));
            sql.push_sql(format!(" AND CHAR_LENGTH({ident}) = CHAR_LENGTH("));
            sql.push_value(text(&request.find));
            sql.push_sql(format!(") AND REPLACE({ident}, "));
            sql.push_value(text(&request.find));
            sql.push_sql(", '') = ''");
            sql
        }
        (ReplaceMode::Whole, _) => {
            let mut sql = Fragment::new(format!("{} = ", column_text(driver, ident)));
            sql.push_value(text(&request.find));
            if driver == Driver::Sqlite {
                sql.push_sql(" COLLATE BINARY");
            }
            sql
        }
    }
}

fn where_clause(driver: Driver, ident: &str, request: &ReplaceRequest, filter: Option<Fragment>) -> Fragment {
    let mut sql = Fragment::new(" WHERE ");
    if let Some(filter) = filter {
        sql.push_sql("(");
        sql.append(filter);
        sql.push_sql(") AND ");
    }
    sql.append(match_condition(driver, ident, request));
    sql
}

/** `table` is already qualified and quoted; `filter` is the compiled filter condition, if any. */
pub fn statements(driver: Driver, table: &str, request: &ReplaceRequest, filter: Option<Fragment>) -> ReplaceStatements {
    let ident = dialect(driver).quote_ident(&request.column);
    let condition = where_clause(driver, &ident, request, filter);

    let mut count = Fragment::new(format!("SELECT COUNT(*) FROM {table}"));
    count.append(condition.clone());

    let mut sample = Fragment::new(format!("SELECT {ident} FROM {table}"));
    sample.append(condition.clone());
    sample.push_sql(format!(" LIMIT {SAMPLE_LIMIT}"));

    let mut update = Fragment::new(format!("UPDATE {table} SET {ident} = "));
    match request.mode {
        ReplaceMode::Contains => update.append(replace_expr(driver, &ident, request)),
        ReplaceMode::Whole => update.push_value(text(&request.replace)),
    }
    update.append(condition);

    ReplaceStatements { count, sample, update }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite;
    use crate::db::CellValue;
    use sqlx::{Connection, SqliteConnection};

    fn request(mode: ReplaceMode, find: &str, replace: &str) -> ReplaceRequest {
        ReplaceRequest {
            namespace: "main".into(),
            table: "posts".into(),
            column: "body".into(),
            find: find.into(),
            replace: replace.into(),
            mode,
            filter: None,
            utc_offset: None,
        }
    }

    #[test]
    fn builds_contains_updates_for_each_driver() {
        let contains = request(ReplaceMode::Contains, "http://", "https://");
        assert_eq!(
            statements(Driver::Mysql, "`app`.`posts`", &contains, None).update.inline(Driver::Mysql),
            "UPDATE `app`.`posts` SET `body` = REPLACE(`body`, 'http://', 'https://') \
             WHERE CAST(REPLACE(`body`, 'http://', 'https://') AS BINARY) <> CAST(`body` AS BINARY)"
        );
        assert_eq!(
            statements(Driver::Postgres, "\"public\".\"posts\"", &contains, None).update.inline(Driver::Postgres),
            "UPDATE \"public\".\"posts\" SET \"body\" = REPLACE(\"body\"::text, E'http://', E'https://') \
             WHERE REPLACE(\"body\"::text, E'http://', E'https://') <> \"body\"::text"
        );
        assert_eq!(
            statements(Driver::Sqlite, "\"main\".\"posts\"", &contains, None).update.inline(Driver::Sqlite),
            "UPDATE \"main\".\"posts\" SET \"body\" = REPLACE(\"body\", 'http://', 'https://') \
             WHERE REPLACE(\"body\", 'http://', 'https://') <> \"body\" COLLATE BINARY"
        );
    }

    #[test]
    fn wraps_the_filter_so_its_or_stays_inside() {
        let whole = request(ReplaceMode::Whole, "draft", "pending");
        let filter = Fragment::new("\"a\" = 1 OR \"b\" = 2");
        assert_eq!(
            statements(Driver::Postgres, "\"posts\"", &whole, Some(filter)).count.inline(Driver::Postgres),
            "SELECT COUNT(*) FROM \"posts\" WHERE (\"a\" = 1 OR \"b\" = 2) AND \"body\"::text = E'draft'"
        );
    }

    #[test]
    fn rejects_requests_that_change_nothing() {
        assert!(check(&request(ReplaceMode::Contains, "", "x"), FilterKind::Text).is_err());
        assert!(check(&request(ReplaceMode::Whole, "a", "a"), FilterKind::Text).is_err());
        assert!(check(&request(ReplaceMode::Contains, "a", "b"), FilterKind::Enum).is_err());
        assert!(check(&request(ReplaceMode::Contains, "a", "b"), FilterKind::Number).is_err());
        assert!(check(&request(ReplaceMode::Whole, "a", "b"), FilterKind::Enum).is_ok());
        assert!(check(&request(ReplaceMode::Contains, "a", "b"), FilterKind::Text).is_ok());
    }

    async fn run(conn: &mut SqliteConnection, statement: &Fragment) -> crate::db::RawOutput {
        let (sql, params) = statement.bound();
        sqlite::run_bound(conn, &sql, &params, 100).await.unwrap()
    }

    async fn bodies(conn: &mut SqliteConnection) -> Vec<CellValue> {
        let output = sqlite::run(conn, "SELECT body FROM posts ORDER BY id", 100, None).await.unwrap();
        output.rows.into_iter().map(|mut row| row.remove(0)).collect()
    }

    async fn posts() -> SqliteConnection {
        let mut conn = SqliteConnection::connect("sqlite::memory:").await.unwrap();
        for sql in [
            "CREATE TABLE posts (id INTEGER PRIMARY KEY, body TEXT COLLATE NOCASE, draft INTEGER)",
            "INSERT INTO posts (body, draft) VALUES ('see http://a and http://b', 0), ('HTTP://c', 0), \
             ('http://d', 1), (NULL, 0), ('plain', 0)",
        ] {
            sqlite::run(&mut conn, sql, 0, None).await.unwrap();
        }
        conn
    }

    #[tokio::test]
    async fn replaces_every_occurrence_case_sensitively_in_filtered_rows() {
        let mut conn = posts().await;
        let contains = request(ReplaceMode::Contains, "http://", "https://");
        let built = statements(Driver::Sqlite, "\"posts\"", &contains, Some(Fragment::new("\"draft\" = 0")));

        assert_eq!(run(&mut conn, &built.count).await.rows[0][0], CellValue::Int(1));
        let sample = run(&mut conn, &built.sample).await;
        let before = sample.rows[0][0].to_text().unwrap();
        assert_eq!(replaced(&contains, &before), "see https://a and https://b");

        assert_eq!(run(&mut conn, &built.update).await.rows_affected, 1);
        assert_eq!(
            bodies(&mut conn).await,
            [
                CellValue::Text("see https://a and https://b".into()),
                CellValue::Text("HTTP://c".into()),
                CellValue::Text("http://d".into()),
                CellValue::Null,
                CellValue::Text("plain".into()),
            ]
        );
    }

    #[tokio::test]
    async fn replaces_only_exact_whole_values() {
        let mut conn = posts().await;
        let whole = request(ReplaceMode::Whole, "plain", "fancy");
        let built = statements(Driver::Sqlite, "\"posts\"", &whole, None);
        sqlite::run(&mut conn, "INSERT INTO posts (body) VALUES ('PLAIN'), ('plain text')", 0, None).await.unwrap();

        assert_eq!(run(&mut conn, &built.update).await.rows_affected, 1);
        let changed = sqlite::run(&mut conn, "SELECT COUNT(*) FROM posts WHERE body = 'fancy' COLLATE BINARY", 1, None)
            .await
            .unwrap();
        assert_eq!(changed.rows[0][0], CellValue::Int(1));
    }
}
