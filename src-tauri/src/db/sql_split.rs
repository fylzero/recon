use crate::models::Driver;

#[derive(Debug, Clone, PartialEq)]
pub struct Statement {
    pub sql: String,
    pub line: usize,
    pub end_line: usize,
}

impl Statement {
    /// Number of source lines in `line..=end_line` spanned by this statement.
    pub fn line_span(&self) -> u64 {
        (self.end_line.saturating_sub(self.line) + 1) as u64
    }
}

#[derive(Debug, Clone, PartialEq)]
enum State {
    Normal,
    Quote { quote: u8, backslash: bool },
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
    recycled: String,
    started: bool,
    state: State,
    line: usize,
    start_line: usize,
    end_line: usize,
}

impl Splitter {
    pub fn new(driver: Driver) -> Self {
        Splitter {
            driver,
            delimiter: ";".into(),
            buf: String::new(),
            recycled: String::new(),
            started: false,
            state: State::Normal,
            line: 0,
            start_line: 0,
            end_line: 0,
        }
    }

    /// Hands back a finished statement's buffer so the next statement can reuse its allocation.
    pub fn recycle(&mut self, mut sql: String) {
        sql.clear();
        if sql.capacity() > self.recycled.capacity() {
            self.recycled = sql;
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
            Err(_) => match mysql_hex_binary_strings(line)
                .filter(|_| self.driver == Driver::Mysql && self.state == State::Normal)
            {
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
        if self.state == State::Normal && self.driver == Driver::Mssql && is_go_batch(line.trim()) {
            self.emit(out);
            return;
        }
        if self.state == State::Normal && !self.started {
            let trimmed = line.trim_start();
            if trimmed.is_empty() {
                return;
            }
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
        let bytes = line.as_bytes();
        let mysql = self.driver == Driver::Mysql;
        let postgres = self.driver == Driver::Postgres;
        let delim_first = self.delimiter.as_bytes()[0];
        let delim_len = self.delimiter.len();
        let mut i = 0;
        let mut copy_from = match &self.state {
            State::Block { keep: false } => None,
            _ if self.started => Some(0),
            _ => None,
        };
        while i < bytes.len() {
            match &self.state {
                State::Quote { quote, backslash } => {
                    let (quote, backslash) = (*quote, *backslash);
                    if !backslash {
                        while let Some(pos) = memchr::memchr(quote, &bytes[i..]) {
                            i += pos;
                            if bytes.get(i + 1) == Some(&quote) {
                                i += 2;
                            } else {
                                i += 1;
                                self.state = State::Normal;
                                break;
                            }
                        }
                        if self.state != State::Normal {
                            i = bytes.len();
                        }
                    } else {
                        while let Some(pos) = memchr::memchr2(quote, b'\\', &bytes[i..]) {
                            i += pos;
                            if bytes[i] == b'\\' {
                                i += 1;
                                if i < bytes.len() {
                                    i += utf8_char_len(bytes[i]);
                                }
                            } else if bytes.get(i + 1) == Some(&quote) {
                                i += 2;
                            } else {
                                i += 1;
                                self.state = State::Normal;
                                break;
                            }
                        }
                        if self.state != State::Normal {
                            i = bytes.len();
                        }
                    }
                }
                State::Block { keep } => {
                    let keep = *keep;
                    if let Some(pos) = find_block_end(&bytes[i..]) {
                        i += pos + 2;
                        self.state = State::Normal;
                        if !keep && self.started {
                            self.buf.push(' ');
                            copy_from = Some(i);
                        }
                    } else {
                        i = bytes.len();
                    }
                }
                State::Dollar(tag) => {
                    if let Some(pos) = memchr::memmem::find(&bytes[i..], tag.as_bytes()) {
                        i += pos + tag.len();
                        self.state = State::Normal;
                    } else {
                        i = bytes.len();
                    }
                }
                State::Normal => {
                    if !self.started {
                        i = skip_whitespace(line, i);
                        if i >= bytes.len() {
                            break;
                        }
                        if bytes[i..].starts_with(self.delimiter.as_bytes()) {
                            i += delim_len;
                            continue;
                        }
                        if bytes[i] == b'-'
                            && bytes.get(i + 1) == Some(&b'-')
                            && (!mysql || is_mysql_dash_comment(line, i))
                        {
                            break;
                        }
                        if mysql && bytes[i] == b'#' {
                            break;
                        }
                        if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
                            let keep = mysql && matches!(bytes.get(i + 2), Some(b'!' | b'+'));
                            if keep {
                                self.started = true;
                                self.start_line = self.line;
                                self.end_line = self.line;
                                copy_from = Some(i);
                            }
                            self.state = State::Block { keep };
                            i += 2;
                            continue;
                        }
                        self.started = true;
                        self.start_line = self.line;
                        self.end_line = self.line;
                        copy_from = Some(i);
                    } else if copy_from.is_none() {
                        copy_from = Some(i);
                    }
                    if delim_len == 1 && delim_first == b';' {
                        while i < bytes.len() && !NORMAL_SPECIAL[bytes[i] as usize] {
                            i += 1;
                        }
                    } else {
                        while i < bytes.len() {
                            let b = bytes[i];
                            if b == delim_first || NORMAL_SPECIAL[b as usize] {
                                break;
                            }
                            i += 1;
                        }
                    }
                    if i >= bytes.len() {
                        break;
                    }
                    if bytes[i..].starts_with(self.delimiter.as_bytes()) {
                        self.end_statement(line, &mut copy_from, i, out);
                        i += delim_len;
                        continue;
                    }
                    match bytes[i] {
                        b'\'' => {
                            let escape = mysql
                                || (postgres && self.escape_string_prefix(line, copy_from, i));
                            self.state = State::Quote {
                                quote: b'\'',
                                backslash: escape,
                            };
                            i += 1;
                        }
                        b'"' => {
                            self.state = State::Quote {
                                quote: b'"',
                                backslash: mysql,
                            };
                            i += 1;
                        }
                        b'`' if !postgres => {
                            self.state = State::Quote {
                                quote: b'`',
                                backslash: false,
                            };
                            i += 1;
                        }
                        b'[' if self.driver == Driver::Mssql => {
                            self.state = State::Quote {
                                quote: b']',
                                backslash: false,
                            };
                            i += 1;
                        }
                        b'-' if bytes.get(i + 1) == Some(&b'-')
                            && (!mysql || is_mysql_dash_comment(line, i)) =>
                        {
                            self.flush_slice(line, &mut copy_from, i);
                            self.buf.push('\n');
                            break;
                        }
                        b'#' if mysql => {
                            self.flush_slice(line, &mut copy_from, i);
                            self.buf.push('\n');
                            break;
                        }
                        b'/' if bytes.get(i + 1) == Some(&b'*') => {
                            let keep = mysql && matches!(bytes.get(i + 2), Some(b'!' | b'+'));
                            if !keep {
                                self.flush_slice(line, &mut copy_from, i);
                            }
                            self.state = State::Block { keep };
                            i += 2;
                        }
                        b'$' if postgres => match dollar_tag(line, i) {
                            Some(tag) => {
                                i += tag.len();
                                self.state = State::Dollar(tag);
                            }
                            None => i += 1,
                        },
                        _ => i += 1,
                    }
                }
            }
        }
        self.flush_slice(line, &mut copy_from, bytes.len());
    }

    /// Emits whatever is left once the file ends, like a final statement without a delimiter.
    pub fn finish(&mut self, out: &mut Vec<Statement>) {
        self.state = State::Normal;
        self.emit(out);
    }

    fn flush_slice(&mut self, line: &str, copy_from: &mut Option<usize>, upto: usize) {
        if let Some(start) = copy_from.take() {
            if upto > start {
                let slice = &line[start..upto];
                if !slice.trim().is_empty() {
                    self.end_line = self.line;
                }
                self.buf.push_str(slice);
            }
        }
    }

    /// `E'...'` strings are the only Postgres strings where backslash escapes.
    fn escape_string_prefix(&self, line: &str, copy_from: Option<usize>, at: usize) -> bool {
        let pending = match copy_from {
            Some(start) if at > start => &line[start..at],
            _ => "",
        };
        let mut tail = pending.chars().rev().chain(self.buf.chars().rev());
        matches!(tail.next(), Some('E' | 'e')) && !tail.next().is_some_and(is_ident_char)
    }

    /// Inside a SQLite trigger body only `END;` closes the statement.
    fn end_statement(
        &mut self,
        line: &str,
        copy_from: &mut Option<usize>,
        at: usize,
        out: &mut Vec<Statement>,
    ) {
        if self.buf.is_empty() {
            if let Some(start) = *copy_from {
                let slice = &line[start..at];
                if self.driver == Driver::Sqlite && is_trigger(slice) && !ends_with_end(slice) {
                    self.flush_slice(line, copy_from, at);
                    self.buf.push(';');
                    return;
                }
                if self.driver == Driver::Mssql && is_mssql_routine(slice) {
                    self.flush_slice(line, copy_from, at);
                    self.buf.push(';');
                    return;
                }
                *copy_from = None;
                let sql = slice.trim_end();
                if !sql.is_empty() {
                    let mut text = std::mem::take(&mut self.recycled);
                    text.clear();
                    text.push_str(sql);
                    out.push(Statement {
                        sql: text,
                        line: self.start_line,
                        end_line: self.line,
                    });
                }
                self.started = false;
                return;
            }
        }
        self.flush_slice(line, copy_from, at);
        self.end_line = self.line;
        if self.driver == Driver::Sqlite && is_trigger(&self.buf) && !ends_with_end(&self.buf) {
            self.buf.push(';');
            return;
        }
        if self.driver == Driver::Mssql && is_mssql_routine(&self.buf) {
            self.buf.push(';');
            return;
        }
        self.emit(out);
    }

    fn emit(&mut self, out: &mut Vec<Statement>) {
        let trimmed_len = self.buf.trim_end().len();
        self.buf.truncate(trimmed_len);
        if is_go_batch(self.buf.trim()) {
            self.buf.clear();
            self.started = false;
            return;
        }
        if !self.buf.is_empty() {
            let sql = std::mem::replace(&mut self.buf, std::mem::take(&mut self.recycled));
            self.buf.clear();
            out.push(Statement {
                sql,
                line: self.start_line,
                end_line: self.end_line.max(self.start_line),
            });
        } else {
            self.buf.clear();
        }
        self.started = false;
    }
}

fn is_go_batch(line: &str) -> bool {
    let mut parts = line.split_whitespace();
    let Some(word) = parts.next() else {
        return false;
    };
    word.eq_ignore_ascii_case("GO")
        && match parts.next() {
            None => true,
            Some(count) => count.chars().all(|c| c.is_ascii_digit()) && parts.next().is_none(),
        }
}

/// A procedure, function, or trigger keeps its internal semicolons until the next `GO`.
fn is_mssql_routine(sql: &str) -> bool {
    super::mssql::defined_module(sql).is_some_and(|kind| kind != "VIEW")
}

const NORMAL_SPECIAL: [bool; 256] = {
    let mut table = [false; 256];
    table[b';' as usize] = true;
    table[b'\'' as usize] = true;
    table[b'"' as usize] = true;
    table[b'`' as usize] = true;
    table[b'[' as usize] = true;
    table[b'-' as usize] = true;
    table[b'/' as usize] = true;
    table[b'#' as usize] = true;
    table[b'$' as usize] = true;
    table
};

fn utf8_char_len(first_byte: u8) -> usize {
    if first_byte < 0x80 {
        1
    } else if first_byte < 0xE0 {
        2
    } else if first_byte < 0xF0 {
        3
    } else {
        4
    }
}

fn skip_whitespace(line: &str, mut i: usize) -> usize {
    let bytes = line.as_bytes();
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i < bytes.len() && !bytes[i].is_ascii() {
        while i < bytes.len() {
            let Some(c) = line[i..].chars().next() else {
                break;
            };
            if c.is_whitespace() {
                i += c.len_utf8();
            } else {
                break;
            }
        }
    }
    i
}

fn is_mysql_dash_comment(line: &str, at: usize) -> bool {
    match line.as_bytes().get(at + 2) {
        None => true,
        Some(b) if b.is_ascii() => b.is_ascii_whitespace(),
        Some(_) => line[at + 2..]
            .chars()
            .next()
            .is_none_or(char::is_whitespace),
    }
}

fn find_block_end(bytes: &[u8]) -> Option<usize> {
    memchr::memmem::find(bytes, b"*/")
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn starts_with_word(text: &str, word: &str) -> bool {
    text.len() >= word.len()
        && text[..word.len()].eq_ignore_ascii_case(word)
        && text[word.len()..]
            .chars()
            .next()
            .is_none_or(char::is_whitespace)
}

/// A `$tag$` opener: letters, digits, and underscores, not starting with a digit.
fn dollar_tag(line: &str, at: usize) -> Option<String> {
    if at > 0 && line[..at].chars().next_back().is_some_and(is_ident_char) {
        return None;
    }
    let rest = &line[at + 1..];
    let mut end_offset = 0;
    for c in rest.chars() {
        if is_ident_char(c) {
            end_offset += c.len_utf8();
        } else {
            break;
        }
    }
    let after = &rest[end_offset..];
    if !after.starts_with('$') || rest.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(line[at..=at + 1 + end_offset].to_string())
}

fn is_trigger(sql: &str) -> bool {
    let bytes = sql.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() {
        return false;
    }
    if (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_')
        && !bytes
            .get(i..i + 6)
            .is_some_and(|w| w.eq_ignore_ascii_case(b"CREATE"))
    {
        return false;
    }
    let mut words = sql[i..]
        .split(|c: char| !is_ident_char(c))
        .filter(|word| !word.is_empty());
    let Some(first) = words.next() else {
        return false;
    };
    if !first.eq_ignore_ascii_case("CREATE") {
        return false;
    }
    let Some(second) = words.next() else {
        return false;
    };
    if second.eq_ignore_ascii_case("TRIGGER") {
        return true;
    }
    (second.eq_ignore_ascii_case("TEMP") || second.eq_ignore_ascii_case("TEMPORARY"))
        && words
            .next()
            .is_some_and(|third| third.eq_ignore_ascii_case("TRIGGER"))
}

fn ends_with_end(sql: &str) -> bool {
    let trimmed = sql.trim_end();
    let start = trimmed
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_ident_char(*c))
        .last()
        .map_or(trimmed.len(), |(index, _)| index);
    if !trimmed[start..].eq_ignore_ascii_case("END") {
        return false;
    }
    let bytes = trimmed.as_bytes();
    let mut i = 0;
    let mut in_body = false;
    let mut case_depth = 0usize;
    let mut closed_body = false;
    while i < bytes.len() {
        let b = bytes[i];
        if matches!(b, b'\'' | b'"' | b'`' | b'[') {
            let close = if b == b'[' { b']' } else { b };
            i += 1;
            while i < bytes.len() {
                if bytes[i] == close {
                    i += 1;
                    if b != b'[' && bytes.get(i) == Some(&close) {
                        i += 1;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
            closed_body = false;
            continue;
        }
        let Some(c) = trimmed[i..].chars().next() else {
            break;
        };
        if is_ident_char(c) {
            let word_start = i;
            i += c.len_utf8();
            while i < bytes.len() {
                let Some(next) = trimmed[i..].chars().next() else {
                    break;
                };
                if is_ident_char(next) {
                    i += next.len_utf8();
                } else {
                    break;
                }
            }
            let word = &trimmed[word_start..i];
            if word.eq_ignore_ascii_case("CASE") {
                case_depth += 1;
                closed_body = false;
            } else if word.eq_ignore_ascii_case("BEGIN") && !in_body {
                in_body = true;
                closed_body = false;
            } else if word.eq_ignore_ascii_case("END") {
                if case_depth > 0 {
                    case_depth -= 1;
                    closed_body = false;
                } else if in_body {
                    in_body = false;
                    closed_body = true;
                } else {
                    closed_body = false;
                }
            } else {
                closed_body = false;
            }
        } else {
            i += c.len_utf8();
        }
    }
    closed_body
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
                    i += if quote == b'"' && line[i] == b'\\' {
                        2
                    } else {
                        1
                    };
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
    let mut words = sql.split_whitespace();
    let Some(first) = words.next() else {
        return false;
    };
    if !first.eq_ignore_ascii_case("COPY") {
        return false;
    }
    let mut prev = first;
    for word in words {
        if prev.eq_ignore_ascii_case("FROM")
            && word.trim_end_matches(';').eq_ignore_ascii_case("STDIN")
        {
            return true;
        }
        prev = word;
    }
    false
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
            vec![
                "INSERT INTO t VALUES ('a;b', \"c;d\")",
                "SELECT 1",
                "SELECT 2"
            ]
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
        assert_eq!(
            split(Driver::Postgres, sql),
            vec!["INSERT INTO t VALUES ('line one;\nline two')"]
        );
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
        let sql = "CREATE TRIGGER stamp AFTER INSERT ON t BEGIN\n  UPDATE t SET at = CASE WHEN NEW.at > 0 THEN 1 ELSE 0 END;\n  DELETE FROM u;\nEND;\nSELECT 1;\n";
        assert_eq!(
            split(Driver::Sqlite, sql),
            vec![
                "CREATE TRIGGER stamp AFTER INSERT ON t BEGIN\n  UPDATE t SET at = CASE WHEN NEW.at > 0 THEN 1 ELSE 0 END;\n  DELETE FROM u;\nEND",
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
        assert_eq!(
            out.iter().map(|s| s.end_line).collect::<Vec<_>>(),
            vec![4, 6]
        );
        assert_eq!(
            out.iter().map(Statement::line_span).collect::<Vec<_>>(),
            vec![2, 1]
        );
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
        assert!(is_copy_from_stdin(
            "COPY public.users (id, name) FROM stdin"
        ));
        assert!(!is_copy_from_stdin("COPY users TO stdout"));
        assert!(!is_copy_from_stdin("SELECT 'COPY x FROM stdin'"));
    }

    #[test]
    fn splits_sql_server_batches_on_go_and_keeps_brackets() {
        let sql = "INSERT INTO [odd;name] VALUES (N'a;b');\nGO\n\
                   CREATE PROCEDURE [dbo].[p] AS BEGIN\n  SELECT 1;\n  SELECT [a]]b];\nEND\nGO 2\n\
                   SELECT 2;\n";
        assert_eq!(
            split(Driver::Mssql, sql),
            vec![
                "INSERT INTO [odd;name] VALUES (N'a;b')",
                "CREATE PROCEDURE [dbo].[p] AS BEGIN\n  SELECT 1;\n  SELECT [a]]b];\nEND",
                "SELECT 2",
            ]
        );
        let commented = "-- Keeps its semicolons\nCREATE OR ALTER PROCEDURE p AS\n  SELECT 1;\n  SELECT 2;\nGO\n";
        assert_eq!(
            split(Driver::Mssql, commented),
            vec!["CREATE OR ALTER PROCEDURE p AS\n  SELECT 1;\n  SELECT 2;"]
        );
    }
}
