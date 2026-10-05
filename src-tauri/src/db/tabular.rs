use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::{BufRead, Write};

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use super::dump::mysql_string;
use super::{dialect, hex, CellValue};
use crate::models::Driver;

const BOM: &[u8] = b"\xEF\xBB\xBF";
const DELIMITERS: [u8; 4] = [b',', b'\t', b';', b'|'];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    #[default]
    Csv,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JsonStyle {
    /// One array holding every row.
    #[default]
    Array,
    /// One object per line (NDJSON).
    Lines,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TabularOptions {
    pub format: Format,
    pub delimiter: char,
    pub header: bool,
    /// How NULL is written in CSV. Text that looks the same is quoted so it reads back as text.
    pub null_as: String,
    /// A UTF-8 byte order mark, which Excel needs to read the file as UTF-8.
    pub bom: bool,
    pub json_style: JsonStyle,
    pub gzip: bool,
}

impl Default for TabularOptions {
    fn default() -> Self {
        TabularOptions {
            format: Format::Csv,
            delimiter: ',',
            header: true,
            null_as: String::new(),
            bom: false,
            json_style: JsonStyle::Array,
            gzip: false,
        }
    }
}

impl TabularOptions {
    pub fn extension(&self) -> &'static str {
        match (self.format, self.json_style, self.gzip) {
            (Format::Csv, _, false) if self.delimiter == '\t' => "tsv",
            (Format::Csv, _, true) if self.delimiter == '\t' => "tsv.gz",
            (Format::Csv, _, false) => "csv",
            (Format::Csv, _, true) => "csv.gz",
            (Format::Json, JsonStyle::Array, false) => "json",
            (Format::Json, JsonStyle::Array, true) => "json.gz",
            (Format::Json, JsonStyle::Lines, false) => "ndjson",
            (Format::Json, JsonStyle::Lines, true) => "ndjson.gz",
        }
    }
}

/// How a cell reads as text in CSV and in JSON strings, or `None` for NULL.
pub fn cell_text(cell: &CellValue) -> Option<String> {
    match cell {
        CellValue::Null => None,
        CellValue::Bool(value) => Some(value.to_string()),
        CellValue::Int(value) => Some(value.to_string()),
        CellValue::Float(value) => Some(value.to_string()),
        CellValue::Bytes(bytes) => Some(format!("\\x{}", hex(bytes))),
        CellValue::Text(text) | CellValue::Json(text) | CellValue::DateTime(text) => Some(text.clone()),
    }
}

pub fn json_value(cell: &CellValue) -> Value {
    match cell {
        CellValue::Null => Value::Null,
        CellValue::Bool(value) => Value::Bool(*value),
        CellValue::Int(value) => Value::from(*value),
        CellValue::Float(value) => serde_json::Number::from_f64(*value).map_or(Value::Null, Value::Number),
        CellValue::Json(text) => serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.clone())),
        other => cell_text(other).map_or(Value::Null, Value::String),
    }
}

fn push_csv_field(line: &mut String, text: &str, delimiter: char, null_as: &str) {
    let quote = text == null_as
        || text.contains(delimiter)
        || text.contains(['"', '\n', '\r'])
        || text.starts_with(char::is_whitespace)
        || text.ends_with(char::is_whitespace);
    if quote {
        line.push('"');
        line.push_str(&text.replace('"', "\"\""));
        line.push('"');
    } else {
        line.push_str(text);
    }
}

/// Repeated names get `_2`, `_3`, and so on, so every JSON key is distinct.
pub fn unique_names(names: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    names
        .into_iter()
        .map(|name| {
            let mut candidate = name.clone();
            let mut next = 2;
            while !seen.insert(candidate.clone()) {
                candidate = format!("{name}_{next}");
                next += 1;
            }
            candidate
        })
        .collect()
}

/// Writes rows as CSV or JSON once it knows the column names.
pub struct TabularWriter<'a> {
    out: &'a mut (dyn Write + Send),
    options: &'a TabularOptions,
    columns: Option<Vec<String>>,
    rows: u64,
    line: String,
}

impl<'a> TabularWriter<'a> {
    pub fn new(out: &'a mut (dyn Write + Send), options: &'a TabularOptions) -> Self {
        TabularWriter { out, options, columns: None, rows: 0, line: String::new() }
    }

    pub fn has_columns(&self) -> bool {
        self.columns.is_some()
    }

    pub fn rows(&self) -> u64 {
        self.rows
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.out
            .write_all(bytes)
            .map_err(|err| format!("Could not write the export file: {err}"))
    }

    pub fn set_columns(&mut self, names: Vec<String>) -> Result<(), String> {
        if self.columns.is_some() {
            return Ok(());
        }
        match self.options.format {
            Format::Csv => {
                if self.options.bom {
                    self.write(BOM)?;
                }
                if self.options.header && !names.is_empty() {
                    let mut line = String::new();
                    for (index, name) in names.iter().enumerate() {
                        if index > 0 {
                            line.push(self.options.delimiter);
                        }
                        push_csv_field(&mut line, name, self.options.delimiter, &self.options.null_as);
                    }
                    line.push('\n');
                    self.write(line.as_bytes())?;
                }
                self.columns = Some(names);
            }
            Format::Json => {
                if self.options.json_style == JsonStyle::Array {
                    self.write(b"[")?;
                }
                self.columns = Some(unique_names(names));
            }
        }
        Ok(())
    }

    pub fn write_row(&mut self, row: &[CellValue]) -> Result<(), String> {
        let columns = self.columns.as_ref().ok_or("The export started writing rows before it knew the columns.")?;
        let mut line = std::mem::take(&mut self.line);
        line.clear();
        match self.options.format {
            Format::Csv => {
                for (index, cell) in row.iter().enumerate() {
                    if index > 0 {
                        line.push(self.options.delimiter);
                    }
                    match cell_text(cell) {
                        Some(text) => push_csv_field(&mut line, &text, self.options.delimiter, &self.options.null_as),
                        None => line.push_str(&self.options.null_as),
                    }
                }
                line.push('\n');
            }
            Format::Json => {
                if self.options.json_style == JsonStyle::Array {
                    line.push_str(if self.rows == 0 { "\n  " } else { ",\n  " });
                }
                line.push('{');
                for (index, (name, cell)) in columns.iter().zip(row).enumerate() {
                    if index > 0 {
                        line.push(',');
                    }
                    line.push_str(&serde_json::to_string(name).unwrap_or_default());
                    line.push(':');
                    line.push_str(&serde_json::to_string(&json_value(cell)).unwrap_or_else(|_| "null".into()));
                }
                line.push('}');
                if self.options.json_style == JsonStyle::Lines {
                    line.push('\n');
                }
            }
        }
        let written = self.write(line.as_bytes());
        self.line = line;
        written?;
        self.rows += 1;
        Ok(())
    }

    /// Closes the JSON array if there is one and returns how many rows were written.
    pub fn finish(mut self) -> Result<u64, String> {
        self.set_columns(Vec::new())?;
        if self.options.format == Format::Json && self.options.json_style == JsonStyle::Array {
            let end: &[u8] = if self.rows == 0 { b"]\n" } else { b"\n]\n" };
            self.write(end)?;
        }
        Ok(self.rows)
    }
}

/// Where a row came from in the file, for error messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    Line(u64),
    Item(u64),
}

impl fmt::Display for Position {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Position::Line(line) => write!(f, "Line {line}"),
            Position::Item(item) => write!(f, "Item {item}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub text: String,
    pub quoted: bool,
}

/// Reads RFC 4180 CSV one record at a time, including quoted fields that span lines.
pub struct CsvReader<R> {
    reader: R,
    delimiter: u8,
    line: u64,
    buf: Vec<u8>,
}

impl<R: BufRead> CsvReader<R> {
    pub fn new(reader: R, delimiter: u8) -> Self {
        CsvReader { reader, delimiter, line: 0, buf: Vec::new() }
    }

    /// The next record and the line it starts on. Blank lines are skipped.
    pub fn next_record(&mut self) -> Result<Option<(u64, Vec<Field>)>, String> {
        'record: loop {
            let start = self.line + 1;
            let mut fields = Vec::new();
            let mut field = Vec::new();
            let mut quoted = false;
            let mut in_quotes = false;
            loop {
                self.buf.clear();
                let read = self
                    .reader
                    .read_until(b'\n', &mut self.buf)
                    .map_err(|err| format!("Could not read the file: {err}"))?;
                if read == 0 {
                    if in_quotes {
                        return Err(format!("Line {start}: a quoted field is never closed."));
                    }
                    return Ok(None);
                }
                let mut bytes = &self.buf[..];
                if self.line == 0 {
                    bytes = bytes.strip_prefix(BOM).unwrap_or(bytes);
                }
                self.line += 1;
                let mut index = 0;
                while index < bytes.len() {
                    let byte = bytes[index];
                    if in_quotes {
                        if byte == b'"' {
                            if bytes.get(index + 1) == Some(&b'"') {
                                field.push(b'"');
                                index += 1;
                            } else {
                                in_quotes = false;
                            }
                        } else {
                            field.push(byte);
                        }
                    } else if byte == self.delimiter {
                        fields.push(finish_field(std::mem::take(&mut field), quoted, start)?);
                        quoted = false;
                    } else if byte == b'\n' || (byte == b'\r' && matches!(bytes.get(index + 1), Some(b'\n') | None)) {
                        break;
                    } else if byte == b'"' && field.is_empty() && !quoted {
                        in_quotes = true;
                        quoted = true;
                    } else {
                        field.push(byte);
                    }
                    index += 1;
                }
                if in_quotes {
                    continue;
                }
                fields.push(finish_field(field, quoted, start)?);
                if fields.len() == 1 && !fields[0].quoted && fields[0].text.is_empty() {
                    continue 'record;
                }
                return Ok(Some((start, fields)));
            }
        }
    }
}

fn finish_field(bytes: Vec<u8>, quoted: bool, line: u64) -> Result<Field, String> {
    let text = String::from_utf8(bytes)
        .map_err(|_| format!("Line {line}: the file isn't UTF-8 text. Save it as UTF-8 and try again."))?;
    Ok(Field { text, quoted })
}

/// The delimiter that appears most often outside quotes on the first line, or a comma.
pub fn detect_delimiter(sample: &[u8]) -> u8 {
    let sample = sample.strip_prefix(BOM).unwrap_or(sample);
    let mut counts = [0usize; DELIMITERS.len()];
    let mut in_quotes = false;
    for &byte in sample {
        match byte {
            b'"' => in_quotes = !in_quotes,
            b'\n' if !in_quotes => break,
            _ if !in_quotes => {
                if let Some(slot) = DELIMITERS.iter().position(|&candidate| candidate == byte) {
                    counts[slot] += 1;
                }
            }
            _ => {}
        }
    }
    let (best, count) = counts
        .iter()
        .enumerate()
        .fold((0, 0), |best, (slot, &count)| if count > best.1 { (slot, count) } else { best });
    if count == 0 {
        b','
    } else {
        DELIMITERS[best]
    }
}

/// A JSON object with its keys in file order.
pub struct JsonObject(pub Vec<(String, Value)>);

impl<'de> Deserialize<'de> for JsonObject {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;

        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = JsonObject;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<JsonObject, A::Error> {
                let mut entries = Vec::new();
                while let Some(entry) = map.next_entry::<String, Value>()? {
                    entries.push(entry);
                }
                Ok(JsonObject(entries))
            }
        }

        deserializer.deserialize_map(ObjectVisitor)
    }
}

/// Stands in for an error the row callback already stored, so it isn't decorated with a JSON position.
const STOPPED: &str = "recon: stopped";

enum Stop {
    No,
    Early,
    Failed(String),
}

struct ArrayRows<'f, F> {
    each: &'f mut F,
    stop: &'f mut Stop,
}

impl<'de, F> Visitor<'de> for ArrayRows<'_, F>
where
    F: FnMut(Position, JsonObject) -> Result<bool, String>,
{
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an array of objects")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        let mut item = 0;
        while let Some(object) = seq.next_element::<JsonObject>()? {
            item += 1;
            match (self.each)(Position::Item(item), object) {
                Ok(true) => {}
                Ok(false) => {
                    *self.stop = Stop::Early;
                    return Err(de::Error::custom(STOPPED));
                }
                Err(err) => {
                    *self.stop = Stop::Failed(err);
                    return Err(de::Error::custom(STOPPED));
                }
            }
        }
        Ok(())
    }
}

/**
 * Reads a top-level array of objects, streamed, or one object per line. Stops
 * early when `each` returns false.
 */
pub fn read_json(reader: &mut dyn BufRead, mut each: impl FnMut(Position, JsonObject) -> Result<bool, String>) -> Result<(), String> {
    let first = skip_leading_space(reader)?;
    if first == Some(b'[') {
        let mut stop = Stop::No;
        let mut deserializer = serde_json::Deserializer::from_reader(&mut *reader);
        let read = (&mut deserializer).deserialize_seq(ArrayRows { each: &mut each, stop: &mut stop });
        return match stop {
            Stop::Early => Ok(()),
            Stop::Failed(err) => Err(err),
            Stop::No => read
                .and_then(|()| deserializer.end())
                .map_err(|err| format!("The JSON isn't valid: {err}")),
        };
    }
    let mut line = Vec::new();
    let mut number = 0u64;
    loop {
        line.clear();
        if reader
            .read_until(b'\n', &mut line)
            .map_err(|err| format!("Could not read the file: {err}"))?
            == 0
        {
            return Ok(());
        }
        number += 1;
        let text = std::str::from_utf8(&line)
            .map_err(|_| format!("Line {number}: the file isn't UTF-8 text. Save it as UTF-8 and try again."))?;
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        let object = serde_json::from_str::<JsonObject>(text).map_err(|err| format!("Line {number}: {err}"))?;
        if !each(Position::Line(number), object)? {
            return Ok(());
        }
    }
}

/// Consumes a BOM and leading whitespace, returning the first byte after them.
fn skip_leading_space(reader: &mut dyn BufRead) -> Result<Option<u8>, String> {
    let failed = |err: std::io::Error| format!("Could not read the file: {err}");
    let mut first_chunk = true;
    loop {
        let buf = reader.fill_buf().map_err(failed)?;
        if buf.is_empty() {
            return Ok(None);
        }
        let mut skip = 0;
        if first_chunk && buf.starts_with(BOM) {
            skip = BOM.len();
        }
        first_chunk = false;
        while skip < buf.len() && buf[skip].is_ascii_whitespace() {
            skip += 1;
        }
        if skip < buf.len() {
            let first = buf[skip];
            reader.consume(skip);
            return Ok(Some(first));
        }
        let len = buf.len();
        reader.consume(len);
    }
}

/// The first non-blank byte of `sample`, for telling JSON from CSV.
pub fn looks_like_json(sample: &[u8]) -> bool {
    let sample = sample.strip_prefix(BOM).unwrap_or(sample);
    matches!(sample.iter().find(|byte| !byte.is_ascii_whitespace()), Some(b'[' | b'{'))
}

/// A value read from a file, before it's written out as SQL.
#[derive(Debug, Clone, PartialEq)]
pub enum ImportValue {
    Null,
    Text(String),
    Bool(bool),
    Number(String),
    /// A nested object or array, as JSON text.
    Json(String),
}

impl ImportValue {
    pub fn from_field(field: Field, null_markers: &[String]) -> Self {
        if !field.quoted && null_markers.iter().any(|marker| *marker == field.text) {
            ImportValue::Null
        } else {
            ImportValue::Text(field.text)
        }
    }

    pub fn from_json(value: Value) -> Self {
        match value {
            Value::Null => ImportValue::Null,
            Value::Bool(value) => ImportValue::Bool(value),
            Value::Number(number) => ImportValue::Number(number.to_string()),
            Value::String(text) => ImportValue::Text(text),
            other => ImportValue::Json(other.to_string()),
        }
    }

    pub fn preview(&self) -> Option<String> {
        match self {
            ImportValue::Null => None,
            ImportValue::Bool(value) => Some(value.to_string()),
            ImportValue::Text(text) | ImportValue::Number(text) | ImportValue::Json(text) => Some(text.clone()),
        }
    }

    /// Roughly how many bytes the value adds to an INSERT, for keeping statements small.
    pub fn size(&self) -> usize {
        match self {
            ImportValue::Null | ImportValue::Bool(_) => 5,
            ImportValue::Text(text) | ImportValue::Number(text) | ImportValue::Json(text) => text.len() + 4,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SourceSpec {
    pub format: Format,
    pub delimiter: u8,
    pub header: bool,
    pub null_markers: Vec<String>,
}

fn header_names(fields: Vec<Field>) -> Vec<String> {
    let names = fields
        .into_iter()
        .enumerate()
        .map(|(index, field)| {
            let name = field.text.trim().to_string();
            if name.is_empty() {
                format!("column_{}", index + 1)
            } else {
                name
            }
        })
        .collect();
    unique_names(names)
}

/**
 * Calls `each` with every row's values lined up with `columns`. With `grow`,
 * columns that turn up along the way are added to `columns`; without it,
 * they're dropped. Missing values are NULL. Stops when `each` returns false.
 */
pub fn read_source(
    reader: &mut dyn BufRead,
    spec: &SourceSpec,
    columns: &mut Vec<String>,
    grow: bool,
    mut each: impl FnMut(Position, Vec<ImportValue>) -> Result<bool, String>,
) -> Result<(), String> {
    match spec.format {
        Format::Csv => {
            let mut csv = CsvReader::new(reader, spec.delimiter);
            if spec.header {
                match csv.next_record()? {
                    Some((_, fields)) if columns.is_empty() => *columns = header_names(fields),
                    Some(_) => {}
                    None => return Ok(()),
                }
            }
            while let Some((line, fields)) = csv.next_record()? {
                if grow {
                    while columns.len() < fields.len() {
                        columns.push(format!("column_{}", columns.len() + 1));
                    }
                }
                let mut values: Vec<ImportValue> = fields
                    .into_iter()
                    .take(columns.len())
                    .map(|field| ImportValue::from_field(field, &spec.null_markers))
                    .collect();
                values.resize(columns.len(), ImportValue::Null);
                if !each(Position::Line(line), values)? {
                    return Ok(());
                }
            }
            Ok(())
        }
        Format::Json => {
            let mut index: HashMap<String, usize> =
                columns.iter().enumerate().map(|(slot, name)| (name.clone(), slot)).collect();
            read_json(reader, |position, object| {
                let mut values = vec![ImportValue::Null; columns.len()];
                for (key, value) in object.0 {
                    let slot = match index.get(&key) {
                        Some(&slot) => slot,
                        None if grow => {
                            columns.push(key.clone());
                            index.insert(key, columns.len() - 1);
                            values.push(ImportValue::Null);
                            columns.len() - 1
                        }
                        None => continue,
                    };
                    values[slot] = ImportValue::from_json(value);
                }
                each(position, values)
            })
        }
    }
}

fn is_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// Numbers with a leading zero, like zip codes, stay text.
fn plain_number(text: &str) -> bool {
    let body = text.strip_prefix(['-', '+']).unwrap_or(text);
    let whole = body.split(['.', 'e', 'E']).next().unwrap_or("");
    !(whole.len() > 1 && whole.starts_with('0'))
}

fn is_int(text: &str) -> bool {
    text.parse::<i64>().is_ok() && plain_number(text)
}

fn is_float(text: &str) -> bool {
    text.bytes().any(|byte| byte.is_ascii_digit())
        && text.bytes().all(|byte| byte.is_ascii_digit() || matches!(byte, b'+' | b'-' | b'.' | b'e' | b'E'))
        && text.parse::<f64>().is_ok_and(f64::is_finite)
        && plain_number(text)
}

fn is_bool(text: &str) -> bool {
    text.eq_ignore_ascii_case("true") || text.eq_ignore_ascii_case("false")
}

fn in_range(text: &str, low: u32, high: u32) -> bool {
    is_digits(text) && text.parse::<u32>().is_ok_and(|value| (low..=high).contains(&value))
}

fn is_date(text: &str) -> bool {
    text.len() == 10
        && text.is_char_boundary(4)
        && text.is_char_boundary(5)
        && text.is_char_boundary(7)
        && text.is_char_boundary(8)
        && is_digits(&text[..4])
        && &text[4..5] == "-"
        && in_range(&text[5..7], 1, 12)
        && &text[7..8] == "-"
        && in_range(&text[8..], 1, 31)
}

/// Whether `text` is a date and time, and whether it ends in a time zone and has fractional seconds.
fn timestamp_parts(text: &str) -> Option<(bool, bool)> {
    if text.len() < 16 || !text.is_char_boundary(10) || !is_date(&text[..10]) {
        return None;
    }
    let rest = text[10..].strip_prefix(['T', ' '])?;
    let (clock, zone) = match rest.find(['Z', 'z', '+', '-']) {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, ""),
    };
    let (clock, fraction) = match clock.split_once('.') {
        Some((clock, fraction)) => (clock, Some(fraction)),
        None => (clock, None),
    };
    let parts: Vec<&str> = clock.split(':').collect();
    let valid_clock = matches!(parts.len(), 2 | 3)
        && in_range(parts[0], 0, 23)
        && parts[0].len() == 2
        && in_range(parts[1], 0, 59)
        && parts[1].len() == 2
        && parts.get(2).is_none_or(|seconds| seconds.len() == 2 && in_range(seconds, 0, 60));
    let valid_fraction = fraction.is_none_or(is_digits);
    let valid_zone = zone.is_empty()
        || zone.eq_ignore_ascii_case("z")
        || zone.len() > 1 && zone[1..].split(':').all(|part| matches!(part.len(), 2 | 4) && is_digits(part));
    (valid_clock && valid_fraction && valid_zone).then_some((!zone.is_empty(), fraction.is_some()))
}

/// Narrows down a column's type as values are seen.
#[derive(Debug, Clone)]
pub struct TypeGuess {
    seen: bool,
    int: bool,
    float: bool,
    boolean: bool,
    date: bool,
    timestamp: bool,
    zoned: bool,
    fraction: bool,
    json: bool,
    longest: usize,
}

impl Default for TypeGuess {
    fn default() -> Self {
        TypeGuess {
            seen: false,
            int: true,
            float: true,
            boolean: true,
            date: true,
            timestamp: true,
            zoned: false,
            fraction: false,
            json: true,
            longest: 0,
        }
    }
}

impl TypeGuess {
    pub fn observe(&mut self, value: &ImportValue) {
        let only = |guess: &mut TypeGuess, int: bool, float: bool, boolean: bool, json: bool| {
            guess.int &= int;
            guess.float &= float;
            guess.boolean &= boolean;
            guess.json &= json;
            guess.date = false;
            guess.timestamp = false;
        };
        match value {
            ImportValue::Null => return,
            ImportValue::Bool(_) => only(self, false, false, true, false),
            ImportValue::Json(text) => {
                self.longest = self.longest.max(text.chars().count());
                only(self, false, false, false, true);
            }
            ImportValue::Number(text) => {
                let int = text.parse::<i64>().is_ok();
                only(self, int, true, false, false);
            }
            ImportValue::Text(text) => {
                self.longest = self.longest.max(text.chars().count());
                self.int &= is_int(text);
                self.float &= is_float(text);
                self.boolean &= is_bool(text);
                self.json = false;
                self.date &= is_date(text);
                match timestamp_parts(text) {
                    Some((zoned, fraction)) => {
                        self.zoned |= zoned;
                        self.fraction |= fraction;
                    }
                    None => self.timestamp = false,
                }
            }
        }
        self.seen = true;
    }

    pub fn sql_type(&self, driver: Driver) -> String {
        let text = match driver {
            Driver::Mysql if self.longest > 16_000 => "LONGTEXT",
            _ => "TEXT",
        };
        if !self.seen {
            return text.into();
        }
        let name = match driver {
            Driver::Postgres if self.boolean => "BOOLEAN",
            Driver::Postgres if self.int => "BIGINT",
            Driver::Postgres if self.float => "DOUBLE PRECISION",
            Driver::Postgres if self.date => "DATE",
            Driver::Postgres if self.timestamp && self.zoned => "TIMESTAMPTZ",
            Driver::Postgres if self.timestamp => "TIMESTAMP",
            Driver::Postgres if self.json => "JSONB",
            Driver::Mysql if self.boolean => "TINYINT(1)",
            Driver::Mysql if self.int => "BIGINT",
            Driver::Mysql if self.float => "DOUBLE",
            Driver::Mysql if self.date => "DATE",
            Driver::Mysql if self.timestamp && !self.zoned && self.fraction => "DATETIME(6)",
            Driver::Mysql if self.timestamp && !self.zoned => "DATETIME",
            Driver::Mysql if self.json => "JSON",
            Driver::Sqlite if self.boolean => "BOOLEAN",
            Driver::Sqlite if self.int => "INTEGER",
            Driver::Sqlite if self.float => "REAL",
            Driver::Sqlite if self.date => "DATE",
            Driver::Sqlite if self.timestamp => "DATETIME",
            _ => text,
        };
        name.into()
    }
}

/// What a column's type means for how imported text is written into it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TargetColumn {
    pub binary: bool,
    pub boolean: bool,
}

impl TargetColumn {
    pub fn of(data_type: &str) -> Self {
        let lower = data_type.trim().to_ascii_lowercase();
        TargetColumn {
            binary: lower.contains("bytea") || lower.contains("blob") || lower.contains("binary"),
            boolean: lower.contains("bool") || lower == "tinyint(1)",
        }
    }
}

/// The hex digits of a `\x`-prefixed value, as exports write bytes.
fn hex_body(text: &str) -> Option<&str> {
    let body = text.strip_prefix("\\x")?;
    (body.len() % 2 == 0 && body.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(body)
}

fn postgres_text(text: &str) -> String {
    format!("E'{}'", text.replace('\\', "\\\\").replace('\'', "''"))
}

fn sqlite_text(text: &str) -> String {
    super::quote_literal(text)
}

/**
 * The SQL for `value` going into a column of `target`'s type. PostgreSQL gets
 * every value as an untyped literal and casts it to the column type itself.
 */
pub fn import_literal(driver: Driver, value: &ImportValue, target: TargetColumn) -> String {
    let as_bool = |value: bool| if value { "1" } else { "0" }.to_string();
    match (driver, value) {
        (_, ImportValue::Null) => "NULL".into(),
        (Driver::Postgres, ImportValue::Bool(value)) => postgres_text(&value.to_string()),
        (Driver::Postgres, ImportValue::Text(text) | ImportValue::Number(text) | ImportValue::Json(text)) => {
            postgres_text(text)
        }
        (_, ImportValue::Bool(value)) => as_bool(*value),
        (_, ImportValue::Number(text)) => text.clone(),
        (_, ImportValue::Text(text)) if target.boolean && is_bool(text) => as_bool(text.eq_ignore_ascii_case("true")),
        (Driver::Mysql, ImportValue::Text(text)) if target.binary && hex_body(text).is_some() => {
            match hex_body(text).unwrap_or_default() {
                "" => "''".into(),
                body => format!("0x{body}"),
            }
        }
        (Driver::Sqlite, ImportValue::Text(text)) if target.binary && hex_body(text).is_some() => {
            format!("X'{}'", hex_body(text).unwrap_or_default())
        }
        (Driver::Mysql, ImportValue::Text(text) | ImportValue::Json(text)) => mysql_string(text),
        (_, ImportValue::Text(text) | ImportValue::Json(text)) => sqlite_text(text),
    }
}

/** `CREATE TABLE` for an import, with an optional auto-increment `id` primary key first. */
pub fn create_table_sql(driver: Driver, table: &str, columns: &[(String, String)], id_column: bool) -> String {
    let quote = |name: &str| dialect(driver).quote_ident(name);
    let mut definitions = Vec::new();
    if id_column {
        definitions.push(match driver {
            Driver::Postgres => format!("{} BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY", quote("id")),
            Driver::Mysql => format!("{} BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY", quote("id")),
            Driver::Sqlite => format!("{} INTEGER PRIMARY KEY AUTOINCREMENT", quote("id")),
        });
    }
    for (name, data_type) in columns {
        definitions.push(format!("{} {}", quote(name), data_type.trim()));
    }
    format!("CREATE TABLE {table} (\n  {}\n)", definitions.join(",\n  "))
}

/// The text before and after the rows of a multi-row INSERT.
pub fn insert_parts(
    driver: Driver,
    table: &str,
    columns: &[String],
    skip_conflicts: bool,
    overriding: bool,
) -> (String, String) {
    let quote = |name: &str| dialect(driver).quote_ident(name);
    let list = columns.iter().map(|name| quote(name)).collect::<Vec<_>>().join(", ");
    match driver {
        Driver::Mysql => {
            let ignore = if skip_conflicts { "IGNORE " } else { "" };
            (format!("INSERT {ignore}INTO {table} ({list}) VALUES "), String::new())
        }
        Driver::Sqlite => {
            let ignore = if skip_conflicts { "OR IGNORE " } else { "" };
            (format!("INSERT {ignore}INTO {table} ({list}) VALUES "), String::new())
        }
        Driver::Postgres => {
            let overriding = if overriding { "OVERRIDING SYSTEM VALUE " } else { "" };
            let conflict = if skip_conflicts { " ON CONFLICT DO NOTHING" } else { "" };
            (format!("INSERT INTO {table} ({list}) {overriding}VALUES "), conflict.to_string())
        }
    }
}

/// One `(…)` row of an INSERT.
pub fn row_values(driver: Driver, values: &[ImportValue], targets: &[TargetColumn]) -> String {
    let mut out = String::from("(");
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        out.push_str(&import_literal(driver, value, targets.get(index).copied().unwrap_or_default()));
    }
    out.push(')');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn csv_options() -> TabularOptions {
        TabularOptions::default()
    }

    fn write_rows(options: &TabularOptions, columns: &[&str], rows: &[Vec<CellValue>]) -> String {
        let mut out: Vec<u8> = Vec::new();
        let mut writer = TabularWriter::new(&mut out, options);
        writer.set_columns(columns.iter().map(|name| name.to_string()).collect()).unwrap();
        for row in rows {
            writer.write_row(row).unwrap();
        }
        writer.finish().unwrap();
        String::from_utf8(out).unwrap()
    }

    fn read_all(text: &str, spec: &SourceSpec) -> (Vec<String>, Vec<Vec<ImportValue>>) {
        let mut columns = Vec::new();
        let mut rows = Vec::new();
        let mut reader = text.as_bytes();
        read_source(&mut reader, spec, &mut columns, true, |_, values| {
            rows.push(values);
            Ok(true)
        })
        .unwrap();
        (columns, rows)
    }

    fn csv_spec() -> SourceSpec {
        SourceSpec { format: Format::Csv, delimiter: b',', header: true, null_markers: vec![String::new()] }
    }

    #[test]
    fn writes_csv_with_quoting_and_nulls() {
        let rows = vec![
            vec![CellValue::Int(1), CellValue::Text("a,b".into()), CellValue::Null],
            vec![CellValue::Int(2), CellValue::Text("say \"hi\"\nthere".into()), CellValue::Text(String::new())],
            vec![CellValue::Bool(true), CellValue::Bytes(vec![0xca, 0xfe]), CellValue::Text(" pad".into())],
        ];
        let csv = write_rows(&csv_options(), &["id", "note", "extra"], &rows);
        assert_eq!(csv, "id,note,extra\n1,\"a,b\",\n2,\"say \"\"hi\"\"\nthere\",\"\"\ntrue,\\xcafe,\" pad\"\n");
    }

    #[test]
    fn quotes_text_that_matches_the_null_marker() {
        let options = TabularOptions { null_as: "\\N".into(), ..csv_options() };
        let csv = write_rows(&options, &["a", "b"], &[vec![CellValue::Null, CellValue::Text("\\N".into())]]);
        assert_eq!(csv, "a,b\n\\N,\"\\N\"\n");
        let spec = SourceSpec { null_markers: vec!["\\N".into()], ..csv_spec() };
        let (_, rows) = read_all(&csv, &spec);
        assert_eq!(rows, vec![vec![ImportValue::Null, ImportValue::Text("\\N".into())]]);
    }

    #[test]
    fn csv_round_trips_empty_strings_and_nulls() {
        let rows = vec![vec![CellValue::Null, CellValue::Text(String::new()), CellValue::Text("x\r\ny".into())]];
        let csv = write_rows(&csv_options(), &["a", "b", "c"], &rows);
        let (columns, read) = read_all(&csv, &csv_spec());
        assert_eq!(columns, vec!["a", "b", "c"]);
        assert_eq!(
            read,
            vec![vec![ImportValue::Null, ImportValue::Text(String::new()), ImportValue::Text("x\r\ny".into())]]
        );
    }

    #[test]
    fn reads_csv_edge_cases() {
        let text = "\u{feff}name,,name\r\n\"multi\nline\",2\r\n\r\n3,4,5,6\nlast,row";
        let (columns, rows) = read_all(text, &csv_spec());
        assert_eq!(columns, vec!["name", "column_2", "name_2", "column_4"]);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0][0], ImportValue::Text("multi\nline".into()));
        assert_eq!(rows[0][2], ImportValue::Null);
        assert_eq!(rows[1][3], ImportValue::Text("6".into()));
        assert_eq!(rows[2][1], ImportValue::Text("row".into()));
    }

    #[test]
    fn reports_line_numbers_and_unclosed_quotes() {
        let mut reader = CsvReader::new("a\nb\n\"c\nd\"\ne".as_bytes(), b',');
        let lines: Vec<u64> = std::iter::from_fn(|| reader.next_record().unwrap().map(|(line, _)| line)).collect();
        assert_eq!(lines, vec![1, 2, 3, 5]);
        let mut broken = CsvReader::new("a\n\"open\nmore".as_bytes(), b',');
        broken.next_record().unwrap();
        assert_eq!(broken.next_record().unwrap_err(), "Line 2: a quoted field is never closed.");
    }

    #[test]
    fn detects_delimiters() {
        assert_eq!(detect_delimiter(b"a;b;c\n1;2;3"), b';');
        assert_eq!(detect_delimiter(b"a\tb\n"), b'\t');
        assert_eq!(detect_delimiter(b"\"x;y\",b,c\n"), b',');
        assert_eq!(detect_delimiter(b"single\n"), b',');
    }

    #[test]
    fn writes_json_arrays_and_lines() {
        let rows = vec![
            vec![CellValue::Int(1), CellValue::Json("{\"k\":[1,2]}".into()), CellValue::Null],
            vec![CellValue::Int(2), CellValue::Text("x".into()), CellValue::Float(1.5)],
        ];
        let array = write_rows(&TabularOptions { format: Format::Json, ..csv_options() }, &["id", "data", "id"], &rows);
        assert_eq!(array, "[\n  {\"id\":1,\"data\":{\"k\":[1,2]},\"id_2\":null},\n  {\"id\":2,\"data\":\"x\",\"id_2\":1.5}\n]\n");
        let lines = write_rows(
            &TabularOptions { format: Format::Json, json_style: JsonStyle::Lines, ..csv_options() },
            &["id"],
            &[vec![CellValue::Int(1)]],
        );
        assert_eq!(lines, "{\"id\":1}\n");
        let empty = write_rows(&TabularOptions { format: Format::Json, ..csv_options() }, &["id"], &[]);
        assert_eq!(empty, "[]\n");
    }

    #[test]
    fn reads_json_arrays_and_lines_in_key_order() {
        let spec = SourceSpec { format: Format::Json, ..csv_spec() };
        let (columns, rows) = read_all("\u{feff} [{\"z\":1,\"a\":\"x\"},{\"a\":null,\"n\":{\"b\":true},\"y\":false}]", &spec);
        assert_eq!(columns, vec!["z", "a", "n", "y"]);
        assert_eq!(
            rows[1],
            vec![ImportValue::Null, ImportValue::Null, ImportValue::Json("{\"b\":true}".into()), ImportValue::Bool(false)]
        );
        let (columns, rows) = read_all("{\"a\":1}\n\n{\"b\":2.5}\n", &spec);
        assert_eq!(columns, vec!["a", "b"]);
        assert_eq!(rows[1], vec![ImportValue::Null, ImportValue::Number("2.5".into())]);
    }

    #[test]
    fn json_errors_say_where() {
        let spec = SourceSpec { format: Format::Json, ..csv_spec() };
        let mut columns = Vec::new();
        let err = read_source(&mut "{\"a\":1}\n[1]\n".as_bytes(), &spec, &mut columns, true, |_, _| Ok(true)).unwrap_err();
        assert!(err.starts_with("Line 2:"), "{err}");
        let err = read_source(&mut "[{\"a\":1},{\"a\":2}]".as_bytes(), &spec, &mut columns, true, |position, _| {
            if position == Position::Item(2) {
                Err("Item 2: boom".into())
            } else {
                Ok(true)
            }
        })
        .unwrap_err();
        assert_eq!(err, "Item 2: boom");
    }

    #[test]
    fn stops_reading_when_asked() {
        let spec = SourceSpec { format: Format::Json, ..csv_spec() };
        let mut seen = 0;
        let mut columns = Vec::new();
        read_source(&mut "[{\"a\":1},{\"a\":2},{\"a\":3}".as_bytes(), &spec, &mut columns, true, |_, _| {
            seen += 1;
            Ok(seen < 2)
        })
        .unwrap();
        assert_eq!(seen, 2);
    }

    fn guess(driver: Driver, values: &[ImportValue]) -> String {
        let mut guess = TypeGuess::default();
        for value in values {
            guess.observe(value);
        }
        guess.sql_type(driver)
    }

    fn texts(values: &[&str]) -> Vec<ImportValue> {
        values.iter().map(|text| ImportValue::Text(text.to_string())).collect()
    }

    #[test]
    fn guesses_column_types() {
        assert_eq!(guess(Driver::Postgres, &texts(&["1", "-20", "300"])), "BIGINT");
        assert_eq!(guess(Driver::Postgres, &texts(&["1", "2.5", "1e3"])), "DOUBLE PRECISION");
        assert_eq!(guess(Driver::Postgres, &texts(&["007", "12"])), "TEXT");
        assert_eq!(guess(Driver::Mysql, &texts(&["true", "FALSE"])), "TINYINT(1)");
        assert_eq!(guess(Driver::Sqlite, &texts(&["2024-02-29", "1999-12-31"])), "DATE");
        assert_eq!(guess(Driver::Postgres, &texts(&["2024-02-29 10:00:00", "2024-03-01T08:30"])), "TIMESTAMP");
        assert_eq!(guess(Driver::Postgres, &texts(&["2024-02-29T10:00:00Z"])), "TIMESTAMPTZ");
        assert_eq!(guess(Driver::Mysql, &texts(&["2024-02-29T10:00:00+02:00"])), "TEXT");
        assert_eq!(guess(Driver::Mysql, &texts(&["2024-02-29 10:00:00.123"])), "DATETIME(6)");
        assert_eq!(guess(Driver::Postgres, &[ImportValue::Json("{}".into()), ImportValue::Null]), "JSONB");
        assert_eq!(guess(Driver::Postgres, &[ImportValue::Number("3".into()), ImportValue::Number("4.5".into())]), "DOUBLE PRECISION");
        assert_eq!(guess(Driver::Postgres, &[ImportValue::Null]), "TEXT");
        assert_eq!(guess(Driver::Postgres, &texts(&["inf"])), "TEXT");
        assert_eq!(guess(Driver::Postgres, &texts(&["1", ""])), "TEXT");
    }

    #[test]
    fn writes_import_literals() {
        let text = |value: &str| ImportValue::Text(value.into());
        let plain = TargetColumn::default();
        let binary = TargetColumn::of("BLOB");
        let boolean = TargetColumn::of("tinyint(1)");
        assert_eq!(import_literal(Driver::Postgres, &text("it's \\ ok"), plain), "E'it''s \\\\ ok'");
        assert_eq!(import_literal(Driver::Postgres, &ImportValue::Bool(true), plain), "E'true'");
        assert_eq!(import_literal(Driver::Mysql, &text("it's"), plain), "'it\\'s'");
        assert_eq!(import_literal(Driver::Mysql, &text("\\xcafe"), binary), "0xcafe");
        assert_eq!(import_literal(Driver::Mysql, &text("\\xcafe"), plain), "'\\\\xcafe'");
        assert_eq!(import_literal(Driver::Mysql, &text("TRUE"), boolean), "1");
        assert_eq!(import_literal(Driver::Sqlite, &text("\\x01"), binary), "X'01'");
        assert_eq!(import_literal(Driver::Sqlite, &ImportValue::Number("2.5".into()), plain), "2.5");
        assert_eq!(import_literal(Driver::Sqlite, &ImportValue::Null, plain), "NULL");
    }

    #[test]
    fn builds_create_and_insert_sql() {
        let columns = vec![("name".to_string(), "TEXT".to_string())];
        assert_eq!(
            create_table_sql(Driver::Sqlite, "\"t\"", &columns, true),
            "CREATE TABLE \"t\" (\n  \"id\" INTEGER PRIMARY KEY AUTOINCREMENT,\n  \"name\" TEXT\n)"
        );
        let names = vec!["a".to_string(), "b".to_string()];
        assert_eq!(
            insert_parts(Driver::Postgres, "\"t\"", &names, true, true),
            ("INSERT INTO \"t\" (\"a\", \"b\") OVERRIDING SYSTEM VALUE VALUES ".to_string(), " ON CONFLICT DO NOTHING".to_string())
        );
        assert_eq!(insert_parts(Driver::Mysql, "`t`", &names, true, false).0, "INSERT IGNORE INTO `t` (`a`, `b`) VALUES ");
    }
}
