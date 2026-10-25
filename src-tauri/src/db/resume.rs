use crate::db::{mysql, quote_literal};
use crate::models::Driver;

/// How a statement changes the open transaction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Tx {
    None,
    Begin,
    Commit,
    Rollback,
    Autocommit(bool),
    /// MySQL commits any open transaction before running DDL and `LOCK TABLES`.
    ImplicitCommit,
}

/// Whether a statement can run a second time without doing its work twice.
#[derive(Debug, Clone, PartialEq)]
pub enum Repeat {
    /// Changes nothing the second time, or fails because the first time already did it.
    Yes,
    /// Adds rows, which fail on the primary key the second time only if the table has one.
    IfKeyed(Table),
    No,
}

/// The table an `INSERT`, `REPLACE`, or `COPY` writes to.
#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    /// The name as written, for Postgres to resolve with its own case folding.
    pub raw: String,
    /// The unquoted parts of the name, like `[schema, table]` or `[table]`.
    pub parts: Vec<String>,
    /// The columns it lists, lowercased unless Postgres quoted them. None when it lists none, meaning all of them.
    pub columns: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Class {
    /// Sets up the connection rather than changing data, so it runs again on a new connection.
    pub session: bool,
    pub tx: Tx,
    pub repeat: Repeat,
}

impl Class {
    fn new(session: bool, tx: Tx, repeat: Repeat) -> Self {
        Class {
            session,
            tx,
            repeat,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Token<'a> {
    Word(&'a str),
    /// A quoted identifier, quotes included.
    Quoted(&'a str),
    Symbol(char),
}

/// The tokens at the start of a statement, reading MySQL's `/*!50003 ... */` as if the comment weren't there.
struct Tokens<'a> {
    sql: &'a str,
    at: usize,
}

impl<'a> Iterator for Tokens<'a> {
    type Item = Token<'a>;

    fn next(&mut self) -> Option<Token<'a>> {
        let bytes = self.sql.as_bytes();
        while self.at < bytes.len() {
            let rest = &bytes[self.at..];
            let byte = rest[0];
            if byte.is_ascii_whitespace() {
                self.at += 1;
            } else if rest.starts_with(b"/*!") {
                self.at += 3;
                while self.at < bytes.len() && bytes[self.at].is_ascii_digit() {
                    self.at += 1;
                }
            } else if rest.starts_with(b"/*") {
                self.at = match self.sql[self.at + 2..].find("*/") {
                    Some(end) => self.at + 2 + end + 2,
                    None => bytes.len(),
                };
            } else if rest.starts_with(b"*/") {
                self.at += 2;
            } else if byte == b'`' || byte == b'"' || byte == b'\'' {
                let start = self.at;
                self.at += 1;
                while self.at < bytes.len() {
                    if bytes[self.at] == byte {
                        if bytes.get(self.at + 1) == Some(&byte) {
                            self.at += 2;
                            continue;
                        }
                        self.at += 1;
                        break;
                    }
                    self.at += if byte == b'\'' && bytes[self.at] == b'\\' {
                        2
                    } else {
                        1
                    };
                }
                self.at = self.at.min(bytes.len());
                return Some(if byte == b'\'' {
                    Token::Symbol('\'')
                } else {
                    Token::Quoted(&self.sql[start..self.at])
                });
            } else if is_word_byte(byte) {
                let start = self.at;
                while self.at < bytes.len() && is_word_byte(bytes[self.at]) {
                    self.at += 1;
                }
                return Some(Token::Word(&self.sql[start..self.at]));
            } else {
                self.at += 1;
                return Some(Token::Symbol(byte as char));
            }
        }
        None
    }
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | b'@') || byte >= 0x80
}

fn tokens(sql: &str) -> Tokens<'_> {
    Tokens { sql, at: 0 }
}

fn is_word(token: Option<&Token>, word: &str) -> bool {
    matches!(token, Some(Token::Word(text)) if text.eq_ignore_ascii_case(word))
}

/// Case-insensitive search for an uppercase ASCII `needle`.
fn contains(haystack: &str, needle: &str) -> bool {
    let (haystack, needle) = (haystack.as_bytes(), needle.as_bytes());
    let first = needle[0];
    haystack.len() >= needle.len()
        && (0..=haystack.len() - needle.len()).any(|at| {
            haystack[at].to_ascii_uppercase() == first
                && haystack[at..at + needle.len()].eq_ignore_ascii_case(needle)
        })
}

/// What resuming needs to know about a statement.
pub fn classify(driver: Driver, sql: &str) -> Class {
    let mysql = driver == Driver::Mysql;
    let head: Vec<Token> = tokens(sql).take(4).collect();
    let first = match head.first() {
        Some(Token::Word(word)) => word.to_ascii_uppercase(),
        _ => return Class::new(false, Tx::None, Repeat::No),
    };
    let ddl = if mysql { Tx::ImplicitCommit } else { Tx::None };
    let second = head.get(1);
    match first.as_str() {
        "SET" => {
            let tx = if mysql {
                mysql_autocommit(sql).map_or(Tx::None, Tx::Autocommit)
            } else {
                Tx::None
            };
            Class::new(true, tx, Repeat::Yes)
        }
        "USE" if mysql => Class::new(true, Tx::None, Repeat::Yes),
        "RESET" if !mysql => Class::new(true, Tx::None, Repeat::Yes),
        "SELECT" if contains(sql, "SET_CONFIG") => Class::new(true, Tx::None, Repeat::Yes),
        "SELECT" if contains(sql, "SETVAL") => Class::new(false, Tx::None, Repeat::Yes),
        "BEGIN" if !(mysql && is_word(second, "NOT")) => Class::new(false, Tx::Begin, Repeat::Yes),
        "START" if is_word(second, "TRANSACTION") => Class::new(false, Tx::Begin, Repeat::Yes),
        "COMMIT" => Class::new(false, Tx::Commit, Repeat::Yes),
        "END" if !mysql => Class::new(false, Tx::Commit, Repeat::Yes),
        "ROLLBACK" | "ABORT" => {
            let partial = head.iter().any(|token| is_word(Some(token), "TO"));
            let tx = if partial { Tx::None } else { Tx::Rollback };
            Class::new(false, tx, Repeat::Yes)
        }
        "SAVEPOINT" | "RELEASE" | "COMMENT" | "SECURITY" | "REINDEX" | "CLUSTER" | "VACUUM" => {
            Class::new(false, Tx::None, Repeat::Yes)
        }
        "CREATE" | "DROP" if is_word(second, "TEMPORARY") || is_word(second, "TEMP") => {
            Class::new(false, Tx::None, Repeat::Yes)
        }
        "CREATE" | "DROP" | "ALTER" | "RENAME" | "TRUNCATE" | "GRANT" | "REVOKE" | "LOCK"
        | "UNLOCK" | "ANALYZE" | "OPTIMIZE" | "REPAIR" | "FLUSH" => {
            Class::new(false, ddl, Repeat::Yes)
        }
        "INSERT" | "REPLACE" => {
            let upsert = contains(sql, "ON DUPLICATE KEY")
                || (contains(sql, "ON CONFLICT") && contains(sql, "DO UPDATE"));
            let repeat = match target(driver, sql) {
                Some(table) if !upsert => Repeat::IfKeyed(table),
                _ => Repeat::No,
            };
            Class::new(false, Tx::None, repeat)
        }
        "COPY" if !mysql && super::sql_split::is_copy_from_stdin(sql) => {
            let repeat = target(driver, sql).map_or(Repeat::No, Repeat::IfKeyed);
            Class::new(false, Tx::None, repeat)
        }
        _ => Class::new(false, Tx::None, Repeat::No),
    }
}

/// The value a MySQL `SET` gives `autocommit`, wherever it is among the assignments.
fn mysql_autocommit(sql: &str) -> Option<bool> {
    let list: Vec<Token> = tokens(sql).take(200).collect();
    list.iter().enumerate().find_map(|(at, token)| {
        let Token::Word(word) = token else {
            return None;
        };
        let name = word.trim_start_matches('@');
        if !name.eq_ignore_ascii_case("autocommit") {
            return None;
        }
        let mut value = list[at + 1..]
            .iter()
            .skip_while(|token| matches!(token, Token::Symbol(':' | '=')));
        match value.next() {
            Some(Token::Word(value)) if value.eq_ignore_ascii_case("ON") || *value == "1" => {
                Some(true)
            }
            Some(Token::Word(value)) if value.eq_ignore_ascii_case("OFF") || *value == "0" => {
                Some(false)
            }
            _ => None,
        }
    })
}

fn unquote(token: &Token) -> Option<String> {
    match token {
        Token::Word(word) => Some(word.to_string()),
        Token::Quoted(quoted) => {
            let quote = &quoted[..1];
            let inner = quoted.strip_prefix(quote)?.strip_suffix(quote)?;
            Some(inner.replace(&quote.repeat(2), quote))
        }
        Token::Symbol(_) => None,
    }
}

/// A column name as the catalog stores it, so it can be matched against the table's primary key.
fn column_name(driver: Driver, token: &Token) -> Option<String> {
    let name = unquote(token)?;
    Some(match (driver, token) {
        (Driver::Postgres, Token::Quoted(_)) => name,
        _ => name.to_lowercase(),
    })
}

/// The table an `INSERT`, `REPLACE`, or `COPY` names, and the columns it lists.
fn target(driver: Driver, sql: &str) -> Option<Table> {
    let mut list = tokens(sql).skip(1).peekable();
    while list.peek().is_some_and(|token| {
        ["LOW_PRIORITY", "DELAYED", "HIGH_PRIORITY", "IGNORE", "INTO"]
            .iter()
            .any(|word| is_word(Some(token), word))
    }) {
        list.next();
    }
    let mut raw = String::new();
    let mut parts = Vec::new();
    loop {
        let token = list.next()?;
        parts.push(unquote(&token)?);
        raw.push_str(match token {
            Token::Word(text) | Token::Quoted(text) => text,
            Token::Symbol(_) => return None,
        });
        if list.peek() != Some(&Token::Symbol('.')) {
            break;
        }
        list.next();
        raw.push('.');
    }
    let mut columns = None;
    if list.peek() == Some(&Token::Symbol('(')) {
        list.next();
        let mut names = Vec::new();
        for token in list.by_ref() {
            match token {
                Token::Symbol(')') => break,
                Token::Symbol(',') => {}
                token => names.push(column_name(driver, &token)?),
            }
        }
        columns = Some(names);
    }
    Some(Table {
        raw,
        parts,
        columns,
    })
}

impl Table {
    /// Lists the table's primary key columns, one per row.
    pub fn primary_key_sql(&self, driver: Driver) -> Option<String> {
        match driver {
            Driver::Mysql => {
                let (schema, name) = match self.parts.as_slice() {
                    [name] => ("DATABASE()".to_string(), mysql::literal(name)),
                    [schema, name] => (mysql::literal(schema), mysql::literal(name)),
                    _ => return None,
                };
                Some(format!(
                    "SELECT COLUMN_NAME FROM information_schema.STATISTICS \
                     WHERE TABLE_SCHEMA = {schema} AND TABLE_NAME = {name} AND INDEX_NAME = 'PRIMARY'"
                ))
            }
            Driver::Postgres => Some(format!(
                "SELECT a.attname FROM pg_index i \
                 JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY(i.indkey) \
                 WHERE i.indrelid = to_regclass({}) AND i.indisprimary",
                quote_literal(&self.raw)
            )),
            Driver::Sqlite => None,
        }
    }

    /// Whether the statement gives every column of `key`, so a second run conflicts with the first.
    pub fn sets_key(&self, driver: Driver, key: &[String]) -> bool {
        let Some(columns) = &self.columns else {
            return !key.is_empty();
        };
        !key.is_empty()
            && key.iter().all(|column| {
                let column = match driver {
                    Driver::Postgres => column.clone(),
                    _ => column.to_lowercase(),
                };
                columns.contains(&column)
            })
    }
}

/// Where a run whose connection dropped can pick up again, in statements from the start of the file.
#[derive(Debug, Clone, PartialEq)]
pub struct Stop {
    /// The first statement that may not have been saved.
    pub from: u64,
    /**
     * The first statement after the ones that may or may not have been saved,
     * or `from` when everything from `from` on certainly wasn't.
     */
    pub to: u64,
    /// The line statement `from` starts on.
    pub line: usize,
    /// False when one of the uncertain statements could do its work twice if it ran again.
    pub repeatable: bool,
    /// Tables the uncertain statements add rows to, which need a primary key for them to run again safely.
    pub tables: Vec<Table>,
}

impl Stop {
    pub fn at(index: u64, line: usize) -> Self {
        Stop {
            from: index,
            to: index,
            line,
            repeatable: true,
            tables: Vec::new(),
        }
    }

    pub fn uncertain(&self) -> bool {
        self.to > self.from
    }

    /// Joins a later drop into this one when both happened before the statements here were known to be saved.
    pub fn absorb(&mut self, later: Stop) {
        self.to = self.to.max(later.to);
        self.repeatable &= later.repeatable;
        for table in later.tables {
            if !self.tables.contains(&table) {
                self.tables.push(table);
            }
        }
    }
}

/**
 * Follows the transactions in a file as it runs, to know which statements
 * are saved for good. Outside a transaction each statement commits on its
 * own. Inside one, nothing is saved until it commits, and a dropped
 * connection rolls all of it back.
 */
pub struct Tracker {
    mysql: bool,
    autocommit: bool,
    in_tx: bool,
    /// The next statement starts a new transaction, as after a COMMIT with autocommit off.
    fresh: bool,
    chunk: Stop,
}

impl Tracker {
    pub fn new(driver: Driver) -> Self {
        Tracker {
            mysql: driver == Driver::Mysql,
            autocommit: true,
            in_tx: false,
            fresh: true,
            chunk: Stop::at(0, 0),
        }
    }

    fn restart(&mut self, index: u64, line: usize) {
        self.chunk = Stop::at(index, line);
        self.fresh = false;
    }

    fn note(&mut self, class: &Class) {
        match &class.repeat {
            Repeat::Yes => {}
            Repeat::No => self.chunk.repeatable = false,
            Repeat::IfKeyed(table) => {
                if !self.chunk.tables.contains(table) {
                    self.chunk.tables.push(table.clone());
                }
            }
        }
    }

    fn committed(&mut self) {
        self.in_tx = !self.autocommit;
        self.fresh = true;
    }

    /// Call before running statement `index`, which starts on `line`.
    pub fn before(&mut self, index: u64, line: usize, class: &Class) {
        if !self.in_tx || self.fresh {
            self.restart(index, line);
        }
        self.note(class);
    }

    /// Call once statement `index` has run.
    pub fn after(&mut self, index: u64, line: usize, class: &Class) {
        match class.tx {
            Tx::Begin => {
                if self.mysql && self.chunk.from != index {
                    self.restart(index, line);
                    self.note(class);
                }
                self.in_tx = true;
            }
            Tx::Commit | Tx::Rollback => self.committed(),
            Tx::ImplicitCommit if self.mysql => self.committed(),
            Tx::Autocommit(true) if !self.autocommit => {
                self.autocommit = true;
                self.committed();
            }
            Tx::Autocommit(false) if self.autocommit => {
                self.autocommit = false;
                if !self.in_tx {
                    self.in_tx = true;
                    self.fresh = true;
                }
            }
            _ => {}
        }
    }

    /// Where to pick up when statement `index` failed because the connection dropped.
    pub fn stop(&self, index: u64, class: &Class) -> Stop {
        let commits = !self.in_tx
            || match class.tx {
                Tx::Commit => true,
                Tx::ImplicitCommit | Tx::Begin => self.mysql,
                Tx::Autocommit(on) => on && !self.autocommit,
                Tx::None | Tx::Rollback => false,
            };
        let mut stop = self.chunk.clone();
        stop.to = if commits { index + 1 } else { stop.from };
        stop
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs `statements` through a tracker as if the last one lost the connection.
    fn stop_at_last(driver: Driver, statements: &[&str]) -> Stop {
        let mut tracker = Tracker::new(driver);
        let (last, done) = statements.split_last().unwrap();
        for (index, sql) in done.iter().enumerate() {
            let class = classify(driver, sql);
            tracker.before(index as u64, index + 1, &class);
            tracker.after(index as u64, index + 1, &class);
        }
        let index = done.len() as u64;
        let class = classify(driver, last);
        tracker.before(index, done.len() + 1, &class);
        tracker.stop(index, &class)
    }

    #[test]
    fn a_statement_outside_a_transaction_may_have_been_saved() {
        let stop = stop_at_last(
            Driver::Mysql,
            &[
                "SET NAMES utf8mb4",
                "INSERT INTO t VALUES (1)",
                "INSERT INTO t VALUES (2)",
            ],
        );
        assert_eq!((stop.from, stop.to, stop.line), (2, 3, 3));
        assert!(stop.repeatable);
        assert_eq!(stop.tables.len(), 1);
    }

    #[test]
    fn a_drop_inside_a_transaction_rolls_back_to_its_start() {
        let stop = stop_at_last(
            Driver::Postgres,
            &[
                "SET x = 1",
                "BEGIN",
                "INSERT INTO t VALUES (1)",
                "INSERT INTO t VALUES (2)",
            ],
        );
        assert_eq!((stop.from, stop.to), (1, 1));
        assert!(!stop.uncertain());
    }

    #[test]
    fn a_drop_while_committing_leaves_the_whole_transaction_uncertain() {
        let stop = stop_at_last(
            Driver::Postgres,
            &[
                "BEGIN",
                "INSERT INTO t VALUES (1)",
                "UPDATE t SET a = 1",
                "COMMIT",
            ],
        );
        assert_eq!((stop.from, stop.to), (0, 4));
        assert!(!stop.repeatable);
    }

    #[test]
    fn mysql_ddl_ends_a_transaction_like_phpmyadmin_exports_rely_on() {
        let stop = stop_at_last(
            Driver::Mysql,
            &[
                "SET SQL_MODE = \"NO_AUTO_VALUE_ON_ZERO\"",
                "START TRANSACTION",
                "CREATE TABLE `t` (`id` int)",
                "INSERT INTO `t` (`id`) VALUES (1)",
                "INSERT INTO `t` (`id`) VALUES (2)",
            ],
        );
        assert_eq!((stop.from, stop.to), (4, 5));
    }

    #[test]
    fn mysql_with_autocommit_off_saves_only_on_commit() {
        let statements = [
            "/*!40101 SET NAMES utf8mb4 */",
            "set autocommit=0",
            "INSERT INTO t VALUES (1)",
            "commit",
            "INSERT INTO t VALUES (2)",
            "INSERT INTO t VALUES (3)",
        ];
        let stop = stop_at_last(Driver::Mysql, &statements);
        assert_eq!((stop.from, stop.to), (4, 4));
        let stop = stop_at_last(Driver::Mysql, &statements[..4]);
        assert_eq!((stop.from, stop.to), (2, 4));
    }

    #[test]
    fn temporary_tables_do_not_commit_in_mysql() {
        let stop = stop_at_last(
            Driver::Mysql,
            &[
                "BEGIN",
                "INSERT INTO t VALUES (1)",
                "CREATE TEMPORARY TABLE x (a int)",
                "INSERT INTO t VALUES (2)",
            ],
        );
        assert_eq!((stop.from, stop.to), (0, 0));
    }

    #[test]
    fn finds_the_statements_that_set_up_the_connection() {
        let session = |driver, sql| classify(driver, sql).session;
        assert!(session(
            Driver::Mysql,
            "/*!40101 SET @OLD_CHARACTER_SET_CLIENT=@@CHARACTER_SET_CLIENT */"
        ));
        assert!(session(Driver::Mysql, "USE `shop`"));
        assert!(session(
            Driver::Postgres,
            "SELECT pg_catalog.set_config('search_path', '', false)"
        ));
        assert!(session(Driver::Postgres, "SET client_encoding = 'UTF8'"));
        assert!(!session(Driver::Mysql, "INSERT INTO t VALUES ('SET x')"));
        assert!(!session(
            Driver::Postgres,
            "SELECT pg_catalog.setval('public.t_id_seq', 10, true)"
        ));
    }

    #[test]
    fn reads_autocommit_from_any_assignment() {
        let tx = |sql| classify(Driver::Mysql, sql).tx;
        assert_eq!(tx("SET autocommit = 0"), Tx::Autocommit(false));
        assert_eq!(
            tx("SET unique_checks=0, @@session.autocommit := ON"),
            Tx::Autocommit(true)
        );
        assert_eq!(tx("SET @@AUTOCOMMIT=1"), Tx::Autocommit(true));
        assert_eq!(tx("SET FOREIGN_KEY_CHECKS=0"), Tx::None);
    }

    #[test]
    fn reads_conditional_comments_and_transaction_keywords() {
        let tx = |driver, sql| classify(driver, sql).tx;
        assert_eq!(
            tx(Driver::Mysql, "/*!40000 ALTER TABLE `t` DISABLE KEYS */"),
            Tx::ImplicitCommit
        );
        assert_eq!(
            tx(Driver::Mysql, "LOCK TABLES `t` WRITE"),
            Tx::ImplicitCommit
        );
        assert_eq!(
            tx(Driver::Postgres, "ALTER TABLE t ADD COLUMN a int"),
            Tx::None
        );
        assert_eq!(tx(Driver::Postgres, "END"), Tx::Commit);
        assert_eq!(tx(Driver::Postgres, "ROLLBACK TO SAVEPOINT a"), Tx::None);
        assert_eq!(tx(Driver::Mysql, "ROLLBACK"), Tx::Rollback);
        assert_eq!(tx(Driver::Mysql, "START TRANSACTION"), Tx::Begin);
    }

    #[test]
    fn parses_the_table_and_columns_rows_go_to() {
        let table = |driver, sql| match classify(driver, sql).repeat {
            Repeat::IfKeyed(table) => Some(table),
            _ => None,
        };
        let mysql = table(
            Driver::Mysql,
            "INSERT IGNORE INTO `shop`.`order items` (`ID`,`name`) VALUES (1,'a')",
        )
        .unwrap();
        assert_eq!(mysql.parts, vec!["shop", "order items"]);
        assert_eq!(
            mysql.columns,
            Some(vec!["id".to_string(), "name".to_string()])
        );
        assert!(mysql.sets_key(Driver::Mysql, &["Id".to_string()]));
        assert!(!mysql.sets_key(Driver::Mysql, &["id".to_string(), "shop_id".to_string()]));
        assert!(!mysql.sets_key(Driver::Mysql, &[]));

        let copy = table(
            Driver::Postgres,
            "COPY public.\"Users\" (id, \"Name\") FROM stdin",
        )
        .unwrap();
        assert_eq!(copy.raw, "public.\"Users\"");
        assert_eq!(
            copy.columns,
            Some(vec!["id".to_string(), "Name".to_string()])
        );

        let all = table(Driver::Postgres, "INSERT INTO public.t VALUES (1, 'x')").unwrap();
        assert_eq!(all.columns, None);
        assert!(all.sets_key(Driver::Postgres, &["id".to_string()]));

        assert_eq!(
            table(
                Driver::Mysql,
                "INSERT INTO t VALUES (1) ON DUPLICATE KEY UPDATE a = a + 1"
            ),
            None
        );
        assert_eq!(
            table(
                Driver::Postgres,
                "INSERT INTO t VALUES (1) ON CONFLICT (id) DO UPDATE SET a = 2"
            ),
            None
        );
        assert_eq!(
            classify(Driver::Mysql, "UPDATE t SET a = a + 1").repeat,
            Repeat::No
        );
    }
}
