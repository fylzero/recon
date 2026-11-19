//! Reads and writes MongoDB values as mongosh-style text.
//!
//! Documents are shown and edited as text like
//! `{ "_id": ObjectId("…"), "count": NumberLong(5), "at": ISODate("…") }`,
//! so a value keeps its exact BSON type through an edit. Plain JSON and
//! Extended JSON (`{"$oid": "…"}`) read the same way, and keys may be left
//! unquoted, as in the shell.

use std::fmt::Write;
use std::str::FromStr;

use mongodb::bson::{
    oid::ObjectId, spec::BinarySubtype, Binary, Bson, DateTime, Decimal128, Document, Regex,
    Timestamp, Uuid,
};

const EXTJSON_KEYS: &[&str] = &[
    "$oid",
    "$date",
    "$numberLong",
    "$numberInt",
    "$numberDouble",
    "$numberDecimal",
    "$binary",
    "$uuid",
    "$regularExpression",
    "$timestamp",
    "$minKey",
    "$maxKey",
    "$undefined",
    "$symbol",
    "$code",
    "$dbPointer",
];

/// Reads one value, such as a document, array, or `ObjectId("…")`.
pub fn parse_value(text: &str) -> Result<Bson, String> {
    let mut parser = Parser::new(text);
    let value = parser.value()?;
    parser.end()?;
    Ok(value)
}

/// Reads a document. Empty text is an empty document.
pub fn parse_document(text: &str) -> Result<Document, String> {
    if text.trim().is_empty() {
        return Ok(Document::new());
    }
    match parse_value(text)? {
        Bson::Document(doc) => Ok(doc),
        other => Err(format!(
            "Expected a document in {{ }}, not {}.",
            type_name(&other)
        )),
    }
}

pub fn type_name(value: &Bson) -> &'static str {
    match value {
        Bson::Double(_) => "a double",
        Bson::String(_) => "a string",
        Bson::Array(_) => "an array",
        Bson::Document(_) => "a document",
        Bson::Boolean(_) => "a boolean",
        Bson::Null => "null",
        Bson::RegularExpression(_) => "a regular expression",
        Bson::Int32(_) => "a 32-bit integer",
        Bson::Int64(_) => "a 64-bit integer",
        Bson::Decimal128(_) => "a decimal",
        Bson::ObjectId(_) => "an ObjectId",
        Bson::DateTime(_) => "a date",
        Bson::Binary(_) => "binary data",
        Bson::Timestamp(_) => "a timestamp",
        _ => "a value",
    }
}

pub struct Parser<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    pub fn new(src: &'a str) -> Self {
        Self { src, pos: 0 }
    }

    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    pub fn peek(&mut self) -> Option<char> {
        self.skip_space();
        self.rest().chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.rest().chars().next()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    pub fn skip_space(&mut self) {
        loop {
            let rest = self.rest();
            let trimmed = rest.trim_start();
            self.pos += rest.len() - trimmed.len();
            if trimmed.starts_with("//") {
                let line = trimmed.find('\n').unwrap_or(trimmed.len());
                self.pos += line;
            } else if let Some(body) = trimmed.strip_prefix("/*") {
                let close = body.find("*/").map(|at| at + 4);
                self.pos += close.unwrap_or(trimmed.len());
            } else {
                return;
            }
        }
    }

    /// Where the parser is, for error messages.
    pub fn here(&self) -> String {
        let consumed = &self.src[..self.pos];
        let line = consumed.matches('\n').count() + 1;
        let column = consumed
            .rsplit('\n')
            .next()
            .map_or(0, |part| part.chars().count())
            + 1;
        if self.src.contains('\n') {
            format!("line {line}, column {column}")
        } else {
            format!("column {column}")
        }
    }

    pub fn error(&self, what: &str) -> String {
        let near: String = self.rest().chars().take(20).collect();
        if near.is_empty() {
            format!("{what} at the end.")
        } else {
            format!("{what} at {} (near “{near}”).", self.here())
        }
    }

    pub fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.bump();
            true
        } else {
            false
        }
    }

    pub fn expect(&mut self, c: char) -> Result<(), String> {
        if self.eat(c) {
            Ok(())
        } else {
            Err(self.error(&format!("Expected “{c}”")))
        }
    }

    pub fn end(&mut self) -> Result<(), String> {
        self.eat(';');
        if self.peek().is_some() {
            return Err(self.error("Unexpected text"));
        }
        Ok(())
    }

    /// A name such as `find` or `ObjectId`. `$` is allowed, as in JavaScript.
    pub fn identifier(&mut self) -> Option<&'a str> {
        self.skip_space();
        let rest = self.rest();
        let mut end = 0;
        for (index, c) in rest.char_indices() {
            let ok = c == '_' || c == '$' || c.is_alphabetic() || (index > 0 && c.is_numeric());
            if !ok {
                break;
            }
            end = index + c.len_utf8();
        }
        if end == 0 {
            return None;
        }
        self.pos += end;
        Some(&rest[..end])
    }

    pub fn value(&mut self) -> Result<Bson, String> {
        match self.peek() {
            None => Err(self.error("Expected a value")),
            Some('{') => self.object(),
            Some('[') => self.array(),
            Some('"') | Some('\'') => self.string().map(Bson::String),
            Some('/') => self.regex(),
            Some(c) if c == '-' || c == '+' || c == '.' || c.is_ascii_digit() => self.number(),
            Some(_) => self.word(),
        }
    }

    fn object(&mut self) -> Result<Bson, String> {
        self.expect('{')?;
        let mut doc = Document::new();
        loop {
            if self.eat('}') {
                break;
            }
            let key = match self.peek() {
                Some('"') | Some('\'') => self.string()?,
                _ => match self.identifier() {
                    Some(name) => name.to_string(),
                    None => {
                        let number = self.number_text();
                        if number.is_empty() {
                            return Err(self.error("Expected a field name"));
                        }
                        number
                    }
                },
            };
            self.expect(':')?;
            let value = self.value()?;
            doc.insert(key, value);
            if !self.eat(',') {
                self.expect('}')?;
                break;
            }
        }
        Ok(extended(doc))
    }

    fn array(&mut self) -> Result<Bson, String> {
        self.expect('[')?;
        let mut items = Vec::new();
        loop {
            if self.eat(']') {
                break;
            }
            items.push(self.value()?);
            if !self.eat(',') {
                self.expect(']')?;
                break;
            }
        }
        Ok(Bson::Array(items))
    }

    pub fn string(&mut self) -> Result<String, String> {
        let quote = match self.peek() {
            Some(c @ ('"' | '\'')) => c,
            _ => return Err(self.error("Expected a string")),
        };
        self.bump();
        let mut out = String::new();
        loop {
            let Some(c) = self.bump() else {
                return Err(self.error("This string isn't closed"));
            };
            match c {
                c if c == quote => return Ok(out),
                '\\' => {
                    let Some(escaped) = self.bump() else {
                        return Err(self.error("This string isn't closed"));
                    };
                    match escaped {
                        'n' => out.push('\n'),
                        't' => out.push('\t'),
                        'r' => out.push('\r'),
                        'b' => out.push('\u{8}'),
                        'f' => out.push('\u{c}'),
                        '0' => out.push('\0'),
                        'u' => out.push(self.unicode_escape()?),
                        other => out.push(other),
                    }
                }
                c => out.push(c),
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let rest = self.rest();
        let digits = rest
            .get(..4)
            .filter(|d| d.chars().all(|c| c.is_ascii_hexdigit()));
        let Some(digits) = digits else {
            return Err(self.error("Expected four hex digits after \\u"));
        };
        self.pos += 4;
        Ok(u32::from_str_radix(digits, 16).unwrap_or(0))
    }

    fn unicode_escape(&mut self) -> Result<char, String> {
        let high = self.hex4()?;
        if (0xD800..0xDC00).contains(&high) && self.rest().starts_with("\\u") {
            self.pos += 2;
            let low = self.hex4()?;
            let code = 0x10000 + ((high - 0xD800) << 10) + (low.wrapping_sub(0xDC00) & 0x3FF);
            return Ok(char::from_u32(code).unwrap_or('\u{FFFD}'));
        }
        Ok(char::from_u32(high).unwrap_or('\u{FFFD}'))
    }

    fn number_text(&mut self) -> String {
        self.skip_space();
        let rest = self.rest();
        let mut end = 0;
        let bytes = rest.as_bytes();
        while end < bytes.len() {
            let c = bytes[end] as char;
            let sign_ok =
                (c == '-' || c == '+') && (end == 0 || matches!(bytes[end - 1] as char, 'e' | 'E'));
            if c.is_ascii_digit() || c == '.' || c == 'e' || c == 'E' || sign_ok {
                end += 1;
            } else {
                break;
            }
        }
        self.pos += end;
        rest[..end].to_string()
    }

    fn number(&mut self) -> Result<Bson, String> {
        let start = self.pos;
        let text = self.number_text();
        if text == "-" || text == "+" {
            if let Some(word) = self.identifier() {
                if word == "Infinity" {
                    let sign = if text == "-" { -1.0 } else { 1.0 };
                    return Ok(Bson::Double(sign * f64::INFINITY));
                }
            }
            self.pos = start;
            return Err(self.error("Expected a number"));
        }
        number_value(&text).ok_or_else(|| {
            self.pos = start;
            self.error("Expected a number")
        })
    }

    fn regex(&mut self) -> Result<Bson, String> {
        self.expect('/')?;
        let mut pattern = String::new();
        let mut in_class = false;
        loop {
            let Some(c) = self.bump() else {
                return Err(self.error("This regular expression isn't closed"));
            };
            match c {
                '\\' => {
                    let Some(next) = self.bump() else {
                        return Err(self.error("This regular expression isn't closed"));
                    };
                    if next != '/' {
                        pattern.push('\\');
                    }
                    pattern.push(next);
                }
                '[' => {
                    in_class = true;
                    pattern.push(c);
                }
                ']' => {
                    in_class = false;
                    pattern.push(c);
                }
                '/' if !in_class => break,
                c => pattern.push(c),
            }
        }
        let rest = self.rest();
        let flags: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphabetic())
            .collect();
        self.pos += flags.len();
        Ok(Bson::RegularExpression(regex(pattern, &flags)?))
    }

    /// Arguments in `( )`, after a function or method name.
    pub fn arguments(&mut self) -> Result<Vec<Bson>, String> {
        self.expect('(')?;
        let mut args = Vec::new();
        loop {
            if self.eat(')') {
                break;
            }
            args.push(self.value()?);
            if !self.eat(',') {
                self.expect(')')?;
                break;
            }
        }
        Ok(args)
    }

    fn word(&mut self) -> Result<Bson, String> {
        let start = self.pos;
        let Some(mut name) = self.identifier() else {
            return Err(self.error("Expected a value"));
        };
        if name == "new" {
            name = self
                .identifier()
                .ok_or_else(|| self.error("Expected a name after new"))?;
        }
        match name {
            "true" => return Ok(Bson::Boolean(true)),
            "false" => return Ok(Bson::Boolean(false)),
            "null" => return Ok(Bson::Null),
            "undefined" => return Ok(Bson::Undefined),
            "NaN" => return Ok(Bson::Double(f64::NAN)),
            "Infinity" => return Ok(Bson::Double(f64::INFINITY)),
            "MinKey" | "MaxKey" if self.peek() != Some('(') => {
                return Ok(if name == "MinKey" {
                    Bson::MinKey
                } else {
                    Bson::MaxKey
                })
            }
            _ => {}
        }
        if self.peek() != Some('(') {
            self.pos = start;
            return Err(self.error("Unknown value"));
        }
        let args = self.arguments()?;
        call(name, args).map_err(|err| {
            self.pos = start;
            self.error(&err)
        })
    }
}

fn number_value(text: &str) -> Option<Bson> {
    let integral = !text.contains(['.', 'e', 'E']);
    if integral {
        if let Ok(value) = text.parse::<i64>() {
            return Some(match i32::try_from(value) {
                Ok(small) => Bson::Int32(small),
                Err(_) => Bson::Int64(value),
            });
        }
    }
    text.parse::<f64>().ok().map(Bson::Double)
}

fn regex(pattern: String, flags: &str) -> Result<Regex, String> {
    if let Some(bad) = flags.chars().find(|c| !"imxslu".contains(*c)) {
        return Err(format!(
            "“{bad}” isn't a regular expression flag MongoDB knows."
        ));
    }
    let mut options: Vec<char> = flags.chars().collect();
    options.sort_unstable();
    options.dedup();
    Ok(Regex {
        pattern,
        options: options.into_iter().collect(),
    })
}

/// Turns an Extended JSON wrapper, such as `{"$oid": "…"}`, into its value.
fn extended(doc: Document) -> Bson {
    let is_wrapper = doc.len() <= 2
        && doc
            .keys()
            .next()
            .is_some_and(|key| EXTJSON_KEYS.contains(&key.as_str()));
    if !is_wrapper {
        return Bson::Document(doc);
    }
    let json = Bson::Document(doc.clone()).into_relaxed_extjson();
    match Bson::try_from(json) {
        Ok(Bson::Document(_)) | Err(_) => Bson::Document(doc),
        Ok(value) => value,
    }
}

fn one_arg(name: &str, mut args: Vec<Bson>) -> Result<Option<Bson>, String> {
    if args.len() > 1 {
        return Err(format!("{name}() takes one argument"));
    }
    Ok(args.pop())
}

fn text_or_number(value: &Bson) -> Option<String> {
    match value {
        Bson::String(text) => Some(text.trim().to_string()),
        Bson::Int32(n) => Some(n.to_string()),
        Bson::Int64(n) => Some(n.to_string()),
        Bson::Double(n) => Some(n.to_string()),
        _ => None,
    }
}

fn call(name: &str, args: Vec<Bson>) -> Result<Bson, String> {
    match name {
        "ObjectId" => match one_arg(name, args)? {
            None => Ok(Bson::ObjectId(ObjectId::new())),
            Some(Bson::String(hex)) => ObjectId::parse_str(hex.trim())
                .map(Bson::ObjectId)
                .map_err(|_| format!("“{hex}” isn't an ObjectId, which is 24 hex characters")),
            Some(_) => Err("ObjectId() takes a string of 24 hex characters".into()),
        },
        "ISODate" | "Date" => match one_arg(name, args)? {
            None => Ok(Bson::DateTime(DateTime::now())),
            Some(Bson::String(text)) => parse_date(&text).map(Bson::DateTime),
            Some(value) => match value.as_i64().or_else(|| value.as_f64().map(|f| f as i64)) {
                Some(millis) => Ok(Bson::DateTime(DateTime::from_millis(millis))),
                None => Err(format!("{name}() takes a date string or milliseconds")),
            },
        },
        "NumberLong" | "Long" => {
            let value = one_arg(name, args)?.unwrap_or(Bson::Int64(0));
            let text = text_or_number(&value).ok_or_else(|| format!("{name}() takes a number"))?;
            text.parse::<i64>()
                .map(Bson::Int64)
                .map_err(|_| format!("“{text}” isn't a 64-bit integer"))
        }
        "NumberInt" | "Int32" => {
            let value = one_arg(name, args)?.unwrap_or(Bson::Int32(0));
            let text = text_or_number(&value).ok_or_else(|| format!("{name}() takes a number"))?;
            text.parse::<i32>()
                .map(Bson::Int32)
                .map_err(|_| format!("“{text}” isn't a 32-bit integer"))
        }
        "Double" | "NumberDouble" => {
            let value = one_arg(name, args)?.unwrap_or(Bson::Double(0.0));
            let text = text_or_number(&value).ok_or_else(|| format!("{name}() takes a number"))?;
            text.parse::<f64>()
                .map(Bson::Double)
                .map_err(|_| format!("“{text}” isn't a number"))
        }
        "NumberDecimal" | "Decimal128" => {
            let value = one_arg(name, args)?.unwrap_or(Bson::String("0".into()));
            let text = text_or_number(&value).ok_or_else(|| format!("{name}() takes a number"))?;
            Decimal128::from_str(&text)
                .map(Bson::Decimal128)
                .map_err(|_| format!("“{text}” isn't a decimal"))
        }
        "UUID" => match one_arg(name, args)? {
            None => Ok(Bson::Binary(Binary::from_uuid(Uuid::new()))),
            Some(Bson::String(text)) => Uuid::parse_str(text.trim())
                .map(|uuid| Bson::Binary(Binary::from_uuid(uuid)))
                .map_err(|_| format!("“{text}” isn't a UUID")),
            Some(_) => Err("UUID() takes a string".into()),
        },
        "BinData" => match args.as_slice() {
            [subtype, Bson::String(base64)] => {
                let subtype = subtype
                    .as_i64()
                    .or_else(|| subtype.as_i32().map(i64::from))
                    .and_then(|n| u8::try_from(n).ok())
                    .ok_or("BinData() takes a subtype from 0 to 255")?;
                Binary::from_base64(base64.trim(), BinarySubtype::from(subtype))
                    .map(Bson::Binary)
                    .map_err(|_| "BinData() takes base64 data".to_string())
            }
            _ => Err("BinData() takes a subtype and a base64 string".into()),
        },
        "Timestamp" => {
            let (time, increment) = match args.as_slice() {
                [Bson::Document(doc)] => (doc.get("t").cloned(), doc.get("i").cloned()),
                [t, i] => (Some(t.clone()), Some(i.clone())),
                [] => (None, None),
                _ => return Err("Timestamp() takes { t, i }".into()),
            };
            let part = |value: Option<Bson>| -> Result<u32, String> {
                match value {
                    None => Ok(0),
                    Some(value) => value
                        .as_i64()
                        .or_else(|| value.as_i32().map(i64::from))
                        .and_then(|n| u32::try_from(n).ok())
                        .ok_or_else(|| "Timestamp() parts are whole numbers".to_string()),
                }
            };
            Ok(Bson::Timestamp(Timestamp {
                time: part(time)?,
                increment: part(increment)?,
            }))
        }
        "RegExp" => match args.as_slice() {
            [Bson::String(pattern)] => Ok(Bson::RegularExpression(regex(pattern.clone(), "")?)),
            [Bson::String(pattern), Bson::String(flags)] => {
                Ok(Bson::RegularExpression(regex(pattern.clone(), flags)?))
            }
            _ => Err("RegExp() takes a pattern and optional flags".into()),
        },
        "MinKey" => Ok(Bson::MinKey),
        "MaxKey" => Ok(Bson::MaxKey),
        _ => Err(format!("{name}() isn't a value Recon knows")),
    }
}

/// Reads a date like `2024-05-01`, `2024-05-01T10:00:00Z`, or `2024-05-01 10:00`.
pub fn parse_date(text: &str) -> Result<DateTime, String> {
    let text = text.trim();
    let mut candidate = text.replacen(' ', "T", 1);
    if candidate.len() == 10 {
        candidate.push_str("T00:00:00Z");
    } else if !candidate.ends_with('Z') && !has_offset(&candidate) {
        if candidate.matches(':').count() == 1 {
            candidate.push_str(":00");
        }
        candidate.push('Z');
    }
    DateTime::parse_rfc3339_str(&candidate).map_err(|_| format!("“{text}” isn't a date"))
}

fn has_offset(text: &str) -> bool {
    let Some(time) = text.split('T').nth(1) else {
        return false;
    };
    time.contains('+') || time.contains('-')
}

fn quote(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| format!("\"{text}\""))
}

fn double_text(value: f64) -> String {
    if value.is_nan() {
        "NaN".into()
    } else if value.is_infinite() {
        if value > 0.0 { "Infinity" } else { "-Infinity" }.into()
    } else if value.fract() == 0.0 && value.abs() < 1e16 {
        format!("{value:.1}")
    } else {
        value.to_string()
    }
}

pub fn date_text(value: DateTime) -> String {
    value
        .try_to_rfc3339_string()
        .unwrap_or_else(|_| value.timestamp_millis().to_string())
}

fn binary_base64(binary: &Binary) -> String {
    let json = Bson::Binary(binary.clone()).into_canonical_extjson();
    json.pointer("/$binary/base64")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string()
}

fn uuid_text(bytes: &[u8]) -> String {
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// A value that isn't a document or array, written as the shell would.
pub fn scalar_text(value: &Bson) -> String {
    match value {
        Bson::Double(n) => double_text(*n),
        Bson::String(text) => quote(text),
        Bson::Boolean(b) => b.to_string(),
        Bson::Null => "null".into(),
        Bson::Undefined => "undefined".into(),
        Bson::Int32(n) => n.to_string(),
        Bson::Int64(n) => format!("NumberLong({n})"),
        Bson::Decimal128(d) => format!("NumberDecimal({})", quote(&d.to_string())),
        Bson::ObjectId(id) => format!("ObjectId({})", quote(&id.to_hex())),
        Bson::DateTime(date) => match date.try_to_rfc3339_string() {
            Ok(text) => format!("ISODate({})", quote(&text)),
            Err(_) => format!("new Date({})", date.timestamp_millis()),
        },
        Bson::Binary(binary)
            if binary.subtype == BinarySubtype::Uuid && binary.bytes.len() == 16 =>
        {
            format!("UUID({})", quote(&uuid_text(&binary.bytes)))
        }
        Bson::Binary(binary) => format!(
            "BinData({}, {})",
            u8::from(binary.subtype),
            quote(&binary_base64(binary))
        ),
        Bson::RegularExpression(regex) => {
            format!("/{}/{}", regex.pattern.replace('/', "\\/"), regex.options)
        }
        Bson::Timestamp(ts) => format!("Timestamp({{ t: {}, i: {} }})", ts.time, ts.increment),
        Bson::MinKey => "MinKey()".into(),
        Bson::MaxKey => "MaxKey()".into(),
        other => other.clone().into_relaxed_extjson().to_string(),
    }
}

/// Writes a value as indented mongosh-style text.
pub fn to_text(value: &Bson) -> String {
    let mut out = String::new();
    write_value(&mut out, value, 0);
    out
}

pub fn document_text(doc: &Document) -> String {
    let mut out = String::new();
    write_document(&mut out, doc, 0);
    out
}

fn indent(out: &mut String, level: usize) {
    for _ in 0..level {
        out.push_str("  ");
    }
}

fn write_value(out: &mut String, value: &Bson, level: usize) {
    match value {
        Bson::Document(doc) => write_document(out, doc, level),
        Bson::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            let short = items.len() <= 8
                && items
                    .iter()
                    .all(|item| !matches!(item, Bson::Document(_) | Bson::Array(_)));
            if short {
                let parts: Vec<String> = items.iter().map(scalar_text).collect();
                let line = format!("[{}]", parts.join(", "));
                if line.len() <= 80 {
                    out.push_str(&line);
                    return;
                }
            }
            out.push_str("[\n");
            for (index, item) in items.iter().enumerate() {
                indent(out, level + 1);
                write_value(out, item, level + 1);
                if index + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            indent(out, level);
            out.push(']');
        }
        other => out.push_str(&scalar_text(other)),
    }
}

fn write_document(out: &mut String, doc: &Document, level: usize) {
    if doc.is_empty() {
        out.push_str("{}");
        return;
    }
    out.push_str("{\n");
    let count = doc.len();
    for (index, (key, value)) in doc.iter().enumerate() {
        indent(out, level + 1);
        let _ = write!(out, "{}: ", quote(key));
        write_value(out, value, level + 1);
        if index + 1 < count {
            out.push(',');
        }
        out.push('\n');
    }
    indent(out, level);
    out.push('}');
}

/// A short one-line version of a value, for grid cells and summaries.
pub fn summary(value: &Bson) -> String {
    match value {
        Bson::Document(doc) => {
            let count = doc.len();
            format!(
                "{{ {count} {} }}",
                if count == 1 { "field" } else { "fields" }
            )
        }
        Bson::Array(items) => {
            let count = items.len();
            format!("[ {count} {} ]", if count == 1 { "item" } else { "items" })
        }
        Bson::String(text) => text.clone(),
        Bson::Int64(n) => n.to_string(),
        Bson::Decimal128(d) => d.to_string(),
        Bson::DateTime(date) => date_text(*date),
        other => scalar_text(other),
    }
}

/// Values as JSON for the frontend: Extended JSON in its relaxed form,
/// except that 64-bit integers past 2^53 stay exact as `{"$numberLong": "…"}`.
pub fn to_json(value: &Bson) -> serde_json::Value {
    const SAFE: i64 = 1 << 53;
    match value {
        Bson::Document(doc) => serde_json::Value::Object(
            doc.iter()
                .map(|(key, value)| (key.clone(), to_json(value)))
                .collect(),
        ),
        Bson::Array(items) => serde_json::Value::Array(items.iter().map(to_json).collect()),
        Bson::Int64(n) if !(-SAFE..=SAFE).contains(n) => {
            serde_json::json!({ "$numberLong": n.to_string() })
        }
        other => other.clone().into_relaxed_extjson(),
    }
}

/// Reads a value the frontend got from `to_json` back.
pub fn from_json(value: serde_json::Value) -> Result<Bson, String> {
    Bson::try_from(value).map_err(|err| format!("Recon couldn't read that value back: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mongodb::bson::doc;

    #[test]
    fn reads_shell_syntax() {
        let doc = parse_document(
            r#"{ name: 'Ada', "age": 36, big: 3000000000, ratio: 1.5, ok: true, gone: null,
                 id: ObjectId("507f1f77bcf86cd799439011"), at: ISODate("2024-05-01T10:00:00Z"),
                 n: NumberLong("7"), tags: ["a", "b",], re: /^a\/b/i, // comment
                 nested: { $gt: 5 } }"#,
        )
        .unwrap();
        assert_eq!(doc.get_str("name").unwrap(), "Ada");
        assert_eq!(doc.get("age"), Some(&Bson::Int32(36)));
        assert_eq!(doc.get("big"), Some(&Bson::Int64(3_000_000_000)));
        assert_eq!(doc.get("ratio"), Some(&Bson::Double(1.5)));
        assert_eq!(doc.get("n"), Some(&Bson::Int64(7)));
        assert!(matches!(doc.get("id"), Some(Bson::ObjectId(_))));
        assert!(matches!(doc.get("at"), Some(Bson::DateTime(_))));
        match doc.get("re") {
            Some(Bson::RegularExpression(re)) => {
                assert_eq!(re.pattern, "^a/b");
                assert_eq!(re.options, "i");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(doc.get_document("nested").unwrap(), &doc! { "$gt": 5 });
    }

    #[test]
    fn reads_extended_json() {
        let doc = parse_document(
            r#"{"_id": {"$oid": "507f1f77bcf86cd799439011"}, "n": {"$numberLong": "9"}, "d": {"$date": "2024-01-01T00:00:00Z"}}"#,
        )
        .unwrap();
        assert!(matches!(doc.get("_id"), Some(Bson::ObjectId(_))));
        assert_eq!(doc.get("n"), Some(&Bson::Int64(9)));
        assert!(matches!(doc.get("d"), Some(Bson::DateTime(_))));
    }

    #[test]
    fn round_trips_types() {
        let original = doc! {
            "_id": ObjectId::parse_str("507f1f77bcf86cd799439011").unwrap(),
            "int": 5,
            "long": 5_i64,
            "double": 5.0,
            "text": "line\n\"quoted\"",
            "date": DateTime::from_millis(1_700_000_000_123),
            "dec": Decimal128::from_str("1.25").unwrap(),
            "uuid": Binary::from_uuid(Uuid::parse_str("a1b2c3d4-e5f6-4789-abcd-ef0123456789").unwrap()),
            "bin": Binary { subtype: BinarySubtype::Generic, bytes: vec![1, 2, 3] },
            "ts": Timestamp { time: 10, increment: 2 },
            "list": [1, "two", { "three": 3 }],
            "empty": {},
            "none": Bson::Null,
            "min": Bson::MinKey,
        };
        let text = document_text(&original);
        assert!(text.contains("NumberLong(5)"));
        assert!(text.contains("5.0"));
        assert_eq!(parse_document(&text).unwrap(), original);
    }

    #[test]
    fn explains_mistakes() {
        let err = parse_document("{ a: 1, b: }").unwrap_err();
        assert!(err.contains("Expected a value"), "{err}");
        assert!(parse_document("[1]").unwrap_err().contains("document"));
        assert!(parse_value("ObjectId('nope')").is_err());
    }

    #[test]
    fn reads_loose_dates() {
        assert!(parse_date("2024-05-01").is_ok());
        assert!(parse_date("2024-05-01 10:30").is_ok());
        assert!(parse_date("2024-05-01T10:30:00+02:00").is_ok());
        assert!(parse_date("yesterday").is_err());
    }

    #[test]
    fn keeps_big_longs_exact_in_json() {
        let json = to_json(&Bson::Int64(9_007_199_254_740_993));
        assert_eq!(from_json(json).unwrap(), Bson::Int64(9_007_199_254_740_993));
        assert_eq!(to_json(&Bson::Int64(5)), serde_json::json!(5));
    }
}
