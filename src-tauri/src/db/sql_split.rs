use crate::models::Driver;

#[derive(Debug, Clone, PartialEq)]
pub struct Statement {
    pub sql: String,
    pub line: usize,
}

#[derive(Debug, Clone, PartialEq)]
enum State {
    Normal,
    Quote { quote: char, backslash: bool },
    Block { keep: bool },
    Dollar(String),
}

/**
 * Splits a SQL file into statements one line at a time, so large dumps never
 * have to fit in memory. Understands quotes, comments, MySQL `DELIMITER`,
 * Postgres dollar quoting, and SQLite trigger bodies. Comments are dropped
 * except MySQL's executable `/*! ... */` and `/*+ ... */` hints.
 */
pub struct Splitter {
    driver: Driver,
    delimiter: String,
    buf: String,
    started: bool,
    state: State,
    line: usize,
    start_line: usize,
}

impl Splitter {
    pub fn new(driver: Driver) -> Self {
        Splitter {
            driver,
            delimiter: ";".into(),
            buf: String::new(),
            started: false,
            state: State::Normal,
            line: 0,
            start_line: 0,
        }
    }

    /// Accounts for lines the caller consumed itself, like `COPY` data.
    pub fn skip_lines(&mut self, count: usize) {
        self.line += count;
    }

    /// Like `push_line`, but for raw bytes. Fails when the line isn't UTF-8 and can't be made so.
    pub fn push_bytes(&mut self, line: &[u8], out: &mut Vec<Statement>) -> Result<(), String> {
        match std::str::from_utf8(line) {
            Ok(text) => self.push_line(text, out),
            Err(_) => match mysql_hex_binary_strings(line).filter(|_| self.driver == Driver::Mysql && self.state == State::Normal) {
                Some(text) => self.push_line(&text, out),
                None => {
                    return Err(format!(
                        "Line {} isn't valid UTF-8. Recon can only import UTF-8 SQL files.",
                        self.line + 1
                    ))
                }
            },
        }
        Ok(())
    }

    pub fn push_line(&mut self, line: &str, out: &mut Vec<Statement>) {
        self.line += 1;
        if self.state == State::Normal && !self.started {
            let trimmed = line.trim();
            if self.driver == Driver::Mysql && starts_with_word(trimmed, "DELIMITER") {
                let delimiter = trimmed["DELIMITER".len()..].trim();
                if !delimiter.is_empty() {
                    self.delimiter = delimiter.to_string();
                }
                return;
            }
            if self.driver == Driver::Postgres && trimmed.starts_with('\\') {
                return;
            }
        }
        let chars: Vec<char> = line.chars().collect();
        let delimiter: Vec<char> = self.delimiter.chars().collect();
        let mysql = self.driver == Driver::Mysql;
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            let next = chars.get(i + 1).copied();
            match &self.state {
                State::Quote { quote, backslash } => {
                    let (quote, backslash) = (*quote, *backslash);
                    self.push(c);
                    if backslash && c == '\\' {
                        if let Some(next) = next {
                            self.push(next);
                            i += 2;
                            continue;
                        }
                    } else if c == quote {
                        if next == Some(quote) {
                            self.push(quote);
                            i += 2;
                            continue;
                        }
                        self.state = State::Normal;
                    }
                    i += 1;
                }
                State::Block { keep } => {
                    let keep = *keep;
                    if c == '*' && next == Some('/') {
                        if keep {
                            self.buf.push_str("*/");
                        } else {
                            self.push(' ');
                        }
                        self.state = State::Normal;
                        i += 2;
                        continue;
                    }
                    if keep {
                        self.push(c);
                    }
                    i += 1;
                }
                State::Dollar(tag) => {
                    let tag = tag.clone();
                    if c == '$' && matches_at(&chars, i, &tag.chars().collect::<Vec<_>>()) {
                        self.buf.push_str(&tag);
                        self.state = State::Normal;
                        i += tag.chars().count();
                        continue;
                    }
                    self.push(c);
                    i += 1;
                }
                State::Normal => {
                    if matches_at(&chars, i, &delimiter) {
                        self.end_statement(out);
                        i += delimiter.len();
                        continue;
                    }
                    match c {
                        '\'' => {
                            let escape = mysql || (self.driver == Driver::Postgres && self.escape_string_prefix());
                            self.state = State::Quote { quote: '\'', backslash: escape };
                            self.push(c);
                        }
                        '"' => {
                            self.state = State::Quote { quote: '"', backslash: mysql };
                            self.push(c);
                        }
                        '`' if self.driver != Driver::Postgres => {
                            self.state = State::Quote { quote: '`', backslash: false };
                            self.push(c);
                        }
                        '-' if next == Some('-')
                            && (!mysql || chars.get(i + 2).is_none_or(|c| c.is_whitespace())) =>
                        {
                            self.push('\n');
                            break;
                        }
                        '#' if mysql => {
                            self.push('\n');
                            break;
                        }
                        '/' if next == Some('*') => {
                            let keep = mysql && matches!(chars.get(i + 2), Some('!' | '+'));
                            if keep {
                                self.push('/');
                                self.push('*');
                            }
                            self.state = State::Block { keep };
                            i += 2;
                            continue;
                        }
                        '$' if self.driver == Driver::Postgres => match dollar_tag(&chars, i) {
                            Some(tag) => {
                                self.push_str(&tag);
                                i += tag.chars().count();
                                self.state = State::Dollar(tag);
                                continue;
                            }
                            None => self.push(c),
                        },
                        _ => self.push(c),
                    }
                    i += 1;
                }
            }
        }
    }

    /// Emits whatever is left once the file ends, like a final statement without a delimiter.
    pub fn finish(&mut self, out: &mut Vec<Statement>) {
        self.state = State::Normal;
        self.emit(out);
    }

    /// Leading whitespace is dropped so a statement's line is where its first token is.
    fn push(&mut self, c: char) {
        if !self.started {
            if c.is_whitespace() {
                return;
            }
            self.started = true;
            self.start_line = self.line;
        }
        self.buf.push(c);
    }

    fn push_str(&mut self, text: &str) {
        for c in text.chars() {
            self.push(c);
        }
    }

    /// `E'...'` strings are the only Postgres strings where backslash escapes.
    fn escape_string_prefix(&self) -> bool {
        let mut tail = self.buf.chars().rev();
        matches!(tail.next(), Some('E' | 'e')) && !tail.next().is_some_and(is_ident_char)
    }

    /// Inside a SQLite trigger body only `END;` closes the statement.
    fn end_statement(&mut self, out: &mut Vec<Statement>) {
        if self.driver == Driver::Sqlite && is_trigger(&self.buf) && !ends_with_end(&self.buf) {
            self.buf.push(';');
            return;
        }
        self.emit(out);
    }

    fn emit(&mut self, out: &mut Vec<Statement>) {
        let sql = self.buf.trim_end();
        if !sql.is_empty() {
            out.push(Statement {
                sql: sql.to_string(),
                line: self.start_line,
            });
        }
        self.buf.clear();
        self.started = false;
    }
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn starts_with_word(text: &str, word: &str) -> bool {
    text.len() >= word.len()
        && text[..word.len()].eq_ignore_ascii_case(word)
        && text[word.len()..].chars().next().is_none_or(char::is_whitespace)
}

fn matches_at(chars: &[char], at: usize, needle: &[char]) -> bool {
    !needle.is_empty() && chars.len() >= at + needle.len() && chars[at..at + needle.len()] == *needle
}

/// A `$tag$` opener: letters, digits, and underscores, not starting with a digit.
fn dollar_tag(chars: &[char], at: usize) -> Option<String> {
    if at > 0 && is_ident_char(chars[at - 1]) {
        return None;
    }
    let mut end = at + 1;
    while end < chars.len() && is_ident_char(chars[end]) {
        end += 1;
    }
    if chars.get(end) != Some(&'$') || chars.get(at + 1).is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(chars[at..=end].iter().collect())
}

fn is_trigger(sql: &str) -> bool {
    let words: Vec<String> = sql
        .split(|c: char| !is_ident_char(c))
        .filter(|word| !word.is_empty())
        .take(3)
        .map(str::to_ascii_uppercase)
        .collect();
    match words.as_slice() {
        [create, trigger, ..] if create == "CREATE" && trigger == "TRIGGER" => true,
        [create, temp, trigger] => create == "CREATE" && (temp == "TEMP" || temp == "TEMPORARY") && trigger == "TRIGGER",
        _ => false,
    }
}

fn ends_with_end(sql: &str) -> bool {
    let trimmed = sql.trim_end();
    let start = trimmed
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_ident_char(*c))
        .last()
        .map_or(trimmed.len(), |(index, _)| index);
    trimmed[start..].eq_ignore_ascii_case("END")
}

/**
 * mysqldump writes binary columns as raw bytes inside quoted strings, which
 * can't be sent as UTF-8 text. This rewrites each such string as the same
 * bytes in a hex literal, leaving valid text strings untouched. Returns None
 * when bytes outside a string literal aren't UTF-8 either.
 */
pub fn mysql_hex_binary_strings(line: &[u8]) -> Option<String> {
    let mut out = String::with_capacity(line.len() * 2);
    let mut i = 0;
    while i < line.len() {
        let start = i;
        match line[i] {
            b'\'' => {
                let mut raw = Vec::new();
                i += 1;
                loop {
                    match *line.get(i)? {
                        b'\\' => {
                            let escaped = *line.get(i + 1)?;
                            match escaped {
                                b'0' => raw.push(0),
                                b'b' => raw.push(8),
                                b'n' => raw.push(b'\n'),
                                b'r' => raw.push(b'\r'),
                                b't' => raw.push(b'\t'),
                                b'Z' => raw.push(0x1a),
                                b'%' | b'_' => raw.extend_from_slice(&[b'\\', escaped]),
                                other => raw.push(other),
                            }
                            i += 2;
                        }
                        b'\'' if line.get(i + 1) == Some(&b'\'') => {
                            raw.push(b'\'');
                            i += 2;
                        }
                        b'\'' => {
                            i += 1;
                            break;
                        }
                        byte => {
                            raw.push(byte);
                            i += 1;
                        }
                    }
                }
                match std::str::from_utf8(&line[start..i]) {
                    Ok(text) => out.push_str(text),
                    Err(_) => out.push_str(&format!("X'{}'", super::hex(&raw))),
                }
            }
            quote @ (b'"' | b'`') => {
                i += 1;
                while *line.get(i)? != quote {
                    i += if quote == b'"' && line[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
                out.push_str(std::str::from_utf8(&line[start..i]).ok()?);
            }
            _ => {
                while i < line.len() && !matches!(line[i], b'\'' | b'"' | b'`') {
                    i += 1;
                }
                out.push_str(std::str::from_utf8(&line[start..i]).ok()?);
            }
        }
    }
    Some(out)
}

/// A Postgres `COPY ... FROM stdin`, whose data follows on the next lines until `\.`.
pub fn is_copy_from_stdin(sql: &str) -> bool {
    let words: Vec<String> = sql.split_whitespace().map(str::to_ascii_uppercase).collect();
    words.first().is_some_and(|word| word == "COPY")
        && words
            .windows(2)
            .any(|pair| pair[0] == "FROM" && pair[1].trim_end_matches(';') == "STDIN")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(driver: Driver, text: &str) -> Vec<String> {
        let mut splitter = Splitter::new(driver);
        let mut out = Vec::new();
        for line in text.split_inclusive('\n') {
            splitter.push_line(line, &mut out);
        }
        splitter.finish(&mut out);
        out.into_iter().map(|statement| statement.sql).collect()
    }

    #[test]
    fn splits_on_semicolons_outside_quotes_and_comments() {
        let sql = "-- header; comment\nINSERT INTO t VALUES ('a;b', \"c;d\");\n/* x; y */ SELECT 1; SELECT 2\n";
        assert_eq!(
            split(Driver::Sqlite, sql),
            vec!["INSERT INTO t VALUES ('a;b', \"c;d\")", "SELECT 1", "SELECT 2"]
        );
    }

    #[test]
    fn handles_mysql_escapes_hash_comments_and_delimiters() {
        let sql = "# note\nINSERT INTO t VALUES ('it\\'s; fine', 'x''y');\n\
                   /*!40101 SET NAMES utf8mb4 */;\n\
                   DELIMITER ;;\nCREATE TRIGGER t BEFORE INSERT ON x FOR EACH ROW BEGIN SET NEW.a = 1; END ;;\nDELIMITER ;\nSELECT 1;\n";
        assert_eq!(
            split(Driver::Mysql, sql),
            vec![
                "INSERT INTO t VALUES ('it\\'s; fine', 'x''y')",
                "/*!40101 SET NAMES utf8mb4 */",
                "CREATE TRIGGER t BEFORE INSERT ON x FOR EACH ROW BEGIN SET NEW.a = 1; END",
                "SELECT 1",
            ]
        );
    }

    #[test]
    fn keeps_multiline_strings_intact() {
        let sql = "INSERT INTO t VALUES ('line one;\nline two');\n";
        assert_eq!(split(Driver::Postgres, sql), vec!["INSERT INTO t VALUES ('line one;\nline two')"]);
    }

    #[test]
    fn understands_postgres_dollar_quotes_and_escape_strings() {
        let sql = "CREATE FUNCTION f() RETURNS int AS $body$ SELECT 1; $body$ LANGUAGE sql;\n\
                   SELECT E'a\\'; b', 'c\\';\n\\connect other\nSELECT $1;\n";
        assert_eq!(
            split(Driver::Postgres, sql),
            vec![
                "CREATE FUNCTION f() RETURNS int AS $body$ SELECT 1; $body$ LANGUAGE sql",
                "SELECT E'a\\'; b', 'c\\'",
                "SELECT $1",
            ]
        );
    }

    #[test]
    fn keeps_sqlite_trigger_bodies_together() {
        let sql = "CREATE TRIGGER stamp AFTER INSERT ON t BEGIN\n  UPDATE t SET at = 1;\n  DELETE FROM u;\nEND;\nSELECT 1;\n";
        assert_eq!(
            split(Driver::Sqlite, sql),
            vec![
                "CREATE TRIGGER stamp AFTER INSERT ON t BEGIN\n  UPDATE t SET at = 1;\n  DELETE FROM u;\nEND",
                "SELECT 1",
            ]
        );
    }

    #[test]
    fn reports_the_line_each_statement_starts_on() {
        let mut splitter = Splitter::new(Driver::Mysql);
        let mut out = Vec::new();
        for line in "\n-- c\nSELECT\n1;\n\nSELECT 2;".split_inclusive('\n') {
            splitter.push_line(line, &mut out);
        }
        splitter.finish(&mut out);
        assert_eq!(out.iter().map(|s| s.line).collect::<Vec<_>>(), vec![3, 6]);
    }

    #[test]
    fn hexes_binary_strings_from_mysqldump() {
        let line = b"INSERT INTO t VALUES (1,_binary '\\0\xca\xfe\\'','caf\xc3\xa9 \\'x\\'',\"a\\\"b\");\n";
        assert_eq!(
            mysql_hex_binary_strings(line).as_deref(),
            Some("INSERT INTO t VALUES (1,_binary X'00cafe27','caf\u{e9} \\'x\\'',\"a\\\"b\");\n")
        );
        assert_eq!(mysql_hex_binary_strings(b"SELECT \xff;"), None);
        assert_eq!(mysql_hex_binary_strings(b"SELECT '\xff"), None);
    }

    #[test]
    fn detects_copy_from_stdin() {
        assert!(is_copy_from_stdin("COPY public.users (id, name) FROM stdin"));
        assert!(!is_copy_from_stdin("COPY users TO stdout"));
        assert!(!is_copy_from_stdin("SELECT 'COPY x FROM stdin'"));
    }
}
