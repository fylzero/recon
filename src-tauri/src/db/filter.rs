use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::{dialect, quote_literal, ColumnDetail, EditValue};
use crate::models::Driver;

/// Leaf conditions after the frontend expands date ranges, so about twice the 50 the UI allows.
pub const MAX_CONDITIONS: usize = 100;
pub const MAX_DEPTH: usize = 4;
pub const MAX_LIST_VALUES: usize = 1000;
pub const MAX_PARAMS: usize = 2000;
pub const MAX_VALUE_LEN: usize = 10_000;
/// `!` needs no quoting in any driver's string literals, unlike the default backslash.
const LIKE_ESCAPE: char = '!';

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FilterKind {
    Text,
    Number,
    Date,
    Datetime,
    Time,
    Boolean,
    Enum,
    Uuid,
    Json,
    Binary,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnClass {
    pub kind: FilterKind,
    /// Postgres `timestamp with time zone`, whose bounds carry the user's UTC offset.
    pub tz: bool,
}

impl ColumnClass {
    fn of(kind: FilterKind) -> Self {
        ColumnClass { kind, tz: false }
    }
}

pub fn classify(driver: Driver, data_type: &str) -> ColumnClass {
    let lower = data_type.trim().to_ascii_lowercase();
    match driver {
        Driver::Mysql => classify_mysql(&lower),
        Driver::Postgres => classify_postgres(&lower),
        Driver::Sqlite => classify_sqlite(&lower),
    }
}

/// Enum columns are classified from their values, since a Postgres enum's type name says nothing.
pub fn column_class(driver: Driver, column: &ColumnDetail) -> ColumnClass {
    let class = classify(driver, &column.data_type);
    if column.enum_values.is_empty() {
        class
    } else {
        ColumnClass::of(FilterKind::Enum)
    }
}

pub fn column_classes(driver: Driver, columns: &[ColumnDetail]) -> HashMap<String, ColumnClass> {
    columns
        .iter()
        .map(|column| (column.name.clone(), column_class(driver, column)))
        .collect()
}

fn base_type(lower: &str) -> &str {
    lower.split('(').next().unwrap_or("").trim()
}

fn classify_mysql(lower: &str) -> ColumnClass {
    use FilterKind::*;
    let base = base_type(lower)
        .trim_end_matches(" zerofill")
        .trim_end_matches(" unsigned")
        .trim();
    if lower.starts_with("tinyint(1)") || matches!(base, "bool" | "boolean") || lower == "bit(1)" || lower == "bit" {
        return ColumnClass::of(Boolean);
    }
    ColumnClass::of(match base {
        "tinyint" | "smallint" | "mediumint" | "int" | "integer" | "bigint" | "year" => Number,
        "decimal" | "numeric" | "dec" | "fixed" | "float" | "double" | "double precision" | "real" => Number,
        "date" => Date,
        "datetime" | "timestamp" => Datetime,
        "time" => Time,
        "char" | "varchar" | "tinytext" | "text" | "mediumtext" | "longtext" | "set" => Text,
        "enum" => Enum,
        "json" => Json,
        "uuid" => Uuid,
        "binary" | "varbinary" | "tinyblob" | "blob" | "mediumblob" | "longblob" | "bit" | "geometry" | "point"
        | "linestring" | "polygon" | "multipoint" | "multilinestring" | "multipolygon" | "geometrycollection"
        | "vector" => Binary,
        _ => Other,
    })
}

fn classify_postgres(lower: &str) -> ColumnClass {
    use FilterKind::*;
    if lower.ends_with("[]") {
        return ColumnClass::of(Other);
    }
    let tz = lower.contains("with time zone");
    let base = base_type(lower);
    match base {
        "smallint" | "integer" | "bigint" | "numeric" | "decimal" | "real" | "double precision" => ColumnClass::of(Number),
        "boolean" => ColumnClass::of(Boolean),
        "text" | "character varying" | "varchar" | "character" | "char" | "bpchar" | "citext" | "name" | "\"char\"" => {
            ColumnClass::of(Text)
        }
        "uuid" => ColumnClass::of(Uuid),
        "json" | "jsonb" => ColumnClass::of(Json),
        "date" => ColumnClass::of(Date),
        "bytea" => ColumnClass::of(Binary),
        _ if base.starts_with("timestamp") => ColumnClass { kind: Datetime, tz },
        _ if base.starts_with("time") && !tz => ColumnClass::of(Time),
        _ => ColumnClass::of(Other),
    }
}

/// SQLite declares types freely, so this follows its column affinity rules plus common date names.
fn classify_sqlite(lower: &str) -> ColumnClass {
    use FilterKind::*;
    ColumnClass::of(if lower.is_empty() {
        Other
    } else if lower.contains("bool") {
        Boolean
    } else if lower.contains("datetime") || lower.contains("timestamp") {
        Datetime
    } else if lower.starts_with("date") {
        Date
    } else if lower.starts_with("time") {
        Time
    } else if lower.contains("int") {
        Number
    } else if lower.contains("char") || lower.contains("clob") || lower.contains("text") || lower.contains("uuid") {
        Text
    } else if lower.contains("blob") {
        Binary
    } else if ["real", "floa", "doub", "numeric", "decimal"].iter().any(|name| lower.contains(name)) {
        Number
    } else if lower.contains("json") {
        Json
    } else {
        Other
    })
}

/** The labels of a MySQL `enum('a','b')` column type, in declaration order. */
pub fn mysql_enum_values(data_type: &str) -> Vec<String> {
    let trimmed = data_type.trim();
    if !trimmed.to_ascii_lowercase().starts_with("enum(") {
        return Vec::new();
    }
    let mut values = Vec::new();
    let mut chars = trimmed[5..].chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\'' {
            continue;
        }
        let mut value = String::new();
        while let Some(c) = chars.next() {
            match c {
                '\'' if chars.peek() == Some(&'\'') => {
                    chars.next();
                    value.push('\'');
                }
                '\'' => break,
                '\\' if chars.peek() == Some(&'\\') => {
                    chars.next();
                    value.push('\\');
                }
                other => value.push(other),
            }
        }
        values.push(value);
    }
    values
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FilterOp {
    Eq,
    Neq,
    Gt,
    Gte,
    Lt,
    Lte,
    Between,
    NotBetween,
    In,
    NotIn,
    Contains,
    NotContains,
    StartsWith,
    EndsWith,
    IsNull,
    IsNotNull,
    IsTrue,
    IsFalse,
    IsEmpty,
    IsNotEmpty,
}

impl FilterOp {
    fn label(self) -> &'static str {
        match self {
            FilterOp::Eq => "is",
            FilterOp::Neq => "is not",
            FilterOp::Gt => "is greater than",
            FilterOp::Gte => "is at least",
            FilterOp::Lt => "is less than",
            FilterOp::Lte => "is at most",
            FilterOp::Between => "is between",
            FilterOp::NotBetween => "is not between",
            FilterOp::In => "is any of",
            FilterOp::NotIn => "is none of",
            FilterOp::Contains => "contains",
            FilterOp::NotContains => "does not contain",
            FilterOp::StartsWith => "starts with",
            FilterOp::EndsWith => "ends with",
            FilterOp::IsNull => "is NULL",
            FilterOp::IsNotNull => "is not NULL",
            FilterOp::IsTrue => "is true",
            FilterOp::IsFalse => "is false",
            FilterOp::IsEmpty => "is empty",
            FilterOp::IsNotEmpty => "is not empty",
        }
    }

    fn arity(self) -> (usize, usize) {
        match self {
            FilterOp::IsNull
            | FilterOp::IsNotNull
            | FilterOp::IsTrue
            | FilterOp::IsFalse
            | FilterOp::IsEmpty
            | FilterOp::IsNotEmpty => (0, 0),
            FilterOp::Between | FilterOp::NotBetween => (2, 2),
            FilterOp::In | FilterOp::NotIn => (1, MAX_LIST_VALUES),
            _ => (1, 1),
        }
    }

    fn is_pattern(self) -> bool {
        matches!(self, FilterOp::Contains | FilterOp::NotContains | FilterOp::StartsWith | FilterOp::EndsWith)
    }
}

pub fn allowed(kind: FilterKind, op: FilterOp) -> bool {
    use FilterKind::*;
    use FilterOp::*;
    if matches!(op, IsNull | IsNotNull) {
        return true;
    }
    match kind {
        Text | Other => matches!(
            op,
            Eq | Neq | Contains | NotContains | StartsWith | EndsWith | In | NotIn | IsEmpty | IsNotEmpty
        ),
        Enum | Uuid => matches!(op, Eq | Neq | In | NotIn),
        Number => matches!(op, Eq | Neq | Gt | Gte | Lt | Lte | Between | NotBetween | In | NotIn),
        Date | Datetime | Time => matches!(op, Eq | Neq | Gt | Gte | Lt | Lte | Between | NotBetween),
        Boolean => matches!(op, IsTrue | IsFalse),
        Json => matches!(op, Contains | NotContains),
        Binary => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MatchMode {
    All,
    Any,
}

/// A filter as the frontend sends it. Relative and whole-day dates arrive already expanded into ranges.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum FilterNode {
    Group {
        #[serde(rename = "match")]
        mode: MatchMode,
        #[serde(default)]
        children: Vec<FilterNode>,
    },
    Condition {
        #[serde(default)]
        id: Option<String>,
        column: String,
        op: FilterOp,
        #[serde(default)]
        values: Vec<EditValue>,
    },
}

#[cfg(test)]
impl FilterNode {
    pub fn all(children: Vec<FilterNode>) -> Self {
        FilterNode::Group { mode: MatchMode::All, children }
    }

    pub fn condition(column: &str, op: FilterOp, values: Vec<EditValue>) -> Self {
        FilterNode::Condition { id: None, column: column.to_string(), op, values }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Part {
    Sql(String),
    Value(EditValue),
    /// A number validated as digits, kept as text so decimals keep their precision.
    Number(String),
}

/**
 * SQL with its values kept apart from the text. `bound` hands the values to
 * the driver as parameters; `inline` writes them as escaped literals for
 * drivers whose rows Recon only decodes from the text protocol.
 */
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Fragment {
    parts: Vec<Part>,
}

impl Fragment {
    pub fn new(sql: impl Into<String>) -> Self {
        Fragment { parts: vec![Part::Sql(sql.into())] }
    }

    pub fn push_sql(&mut self, sql: impl AsRef<str>) {
        match self.parts.last_mut() {
            Some(Part::Sql(text)) => text.push_str(sql.as_ref()),
            _ => self.parts.push(Part::Sql(sql.as_ref().to_string())),
        }
    }

    fn push_value(&mut self, value: EditValue) {
        self.parts.push(Part::Value(value));
    }

    fn push_number(&mut self, number: String) {
        self.parts.push(Part::Number(number));
    }

    pub fn append(&mut self, other: Fragment) {
        for part in other.parts {
            match part {
                Part::Sql(text) => self.push_sql(text),
                part => self.parts.push(part),
            }
        }
    }

    pub fn param_count(&self) -> usize {
        self.parts.iter().filter(|part| !matches!(part, Part::Sql(_))).count()
    }

    /** SQL with `?` placeholders and the values to bind in order. */
    pub fn bound(&self) -> (String, Vec<EditValue>) {
        let mut sql = String::new();
        let mut params = Vec::new();
        for part in &self.parts {
            match part {
                Part::Sql(text) => sql.push_str(text),
                Part::Value(value) => {
                    sql.push('?');
                    params.push(value.clone());
                }
                Part::Number(number) => {
                    sql.push('?');
                    params.push(match number.parse::<i64>() {
                        Ok(value) => EditValue::Int(value),
                        Err(_) => EditValue::Float(number.parse().unwrap_or(0.0)),
                    });
                }
            }
        }
        (sql, params)
    }

    pub fn inline(&self, driver: Driver) -> String {
        let mut sql = String::new();
        for part in &self.parts {
            match part {
                Part::Sql(text) => sql.push_str(text),
                Part::Value(value) => sql.push_str(&literal(driver, value)),
                Part::Number(number) => sql.push_str(number),
            }
        }
        sql
    }
}

pub fn literal(driver: Driver, value: &EditValue) -> String {
    match value {
        EditValue::Null => "NULL".into(),
        EditValue::Bool(value) => match driver {
            Driver::Postgres => if *value { "TRUE" } else { "FALSE" }.into(),
            _ => i64::from(*value).to_string(),
        },
        EditValue::Int(value) => value.to_string(),
        EditValue::Float(value) => value.to_string(),
        EditValue::Text(text) => match driver {
            Driver::Postgres => format!("E'{}'", text.replace('\\', "\\\\").replace('\'', "''")),
            Driver::Mysql => quote_literal(&text.replace('\\', "\\\\")),
            Driver::Sqlite => quote_literal(text),
        },
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FilterError {
    pub id: Option<String>,
    pub message: String,
}

impl FilterError {
    fn new(id: Option<&str>, message: impl Into<String>) -> Self {
        FilterError { id: id.map(str::to_string), message: message.into() }
    }

    /** JSON the frontend reads to show the message on the condition that caused it. */
    pub fn into_message(self) -> String {
        serde_json::json!({ "filterError": { "id": self.id, "message": self.message } }).to_string()
    }
}

pub struct FilterContext<'a> {
    pub driver: Driver,
    pub columns: &'a HashMap<String, ColumnClass>,
    /// Minutes east of UTC on the user's machine.
    pub utc_offset: Option<i32>,
}

/** The WHERE condition for `root`, or `None` when it has no conditions. */
pub fn compile(ctx: &FilterContext, root: &FilterNode) -> Result<Option<Fragment>, FilterError> {
    let (count, depth) = measure(root, 1);
    if count > MAX_CONDITIONS {
        return Err(FilterError::new(None, format!("Use at most {MAX_CONDITIONS} conditions.")));
    }
    if depth > MAX_DEPTH {
        return Err(FilterError::new(None, format!("Groups can be nested at most {MAX_DEPTH} levels deep.")));
    }
    let compiled = compile_node(ctx, root, true)?;
    if compiled.as_ref().is_some_and(|sql| sql.param_count() > MAX_PARAMS) {
        return Err(FilterError::new(None, format!("Filters can use at most {MAX_PARAMS} values in total.")));
    }
    Ok(compiled)
}

fn measure(node: &FilterNode, depth: usize) -> (usize, usize) {
    match node {
        FilterNode::Condition { .. } => (1, depth),
        FilterNode::Group { children, .. } => children.iter().fold((0, depth), |(count, deepest), child| {
            let (child_count, child_depth) = measure(child, depth + 1);
            (count + child_count, deepest.max(child_depth))
        }),
    }
}

fn compile_node(ctx: &FilterContext, node: &FilterNode, top: bool) -> Result<Option<Fragment>, FilterError> {
    match node {
        FilterNode::Condition { id, column, op, values } => {
            compile_condition(ctx, id.as_deref(), column, *op, values).map(Some)
        }
        FilterNode::Group { mode, children } => {
            let mut parts = Vec::new();
            for child in children {
                if let Some(part) = compile_node(ctx, child, false)? {
                    parts.push(part);
                }
            }
            if parts.len() <= 1 {
                return Ok(parts.pop());
            }
            let joiner = match mode {
                MatchMode::All => " AND ",
                MatchMode::Any => " OR ",
            };
            let mut sql = Fragment::new(if top { "" } else { "(" });
            for (index, part) in parts.into_iter().enumerate() {
                if index > 0 {
                    sql.push_sql(joiner);
                }
                sql.append(part);
            }
            if !top {
                sql.push_sql(")");
            }
            Ok(Some(sql))
        }
    }
}

fn compile_condition(
    ctx: &FilterContext,
    id: Option<&str>,
    column: &str,
    op: FilterOp,
    values: &[EditValue],
) -> Result<Fragment, FilterError> {
    let class = ctx
        .columns
        .get(column)
        .copied()
        .ok_or_else(|| FilterError::new(id, format!("This table has no column named “{column}”.")))?;
    if !allowed(class.kind, op) {
        return Err(FilterError::new(id, format!("“{}” can't be used with {column}.", op.label())));
    }
    let (min, max) = op.arity();
    if values.len() < min || values.len() > max {
        let message = match op {
            FilterOp::In | FilterOp::NotIn if values.is_empty() => format!("Add at least one value for {column}."),
            FilterOp::In | FilterOp::NotIn => format!("Use at most {MAX_LIST_VALUES} values for {column}."),
            FilterOp::Between | FilterOp::NotBetween => format!("{column} {} needs two values.", op.label()),
            _ if min == 0 => format!("{column} {} takes no value.", op.label()),
            _ => format!("{column} {} needs one value.", op.label()),
        };
        return Err(FilterError::new(id, message));
    }
    let ident = dialect(ctx.driver).quote_ident(column);
    let postgres = ctx.driver == Driver::Postgres;
    match op {
        FilterOp::IsNull => return Ok(Fragment::new(format!("{ident} IS NULL"))),
        FilterOp::IsNotNull => return Ok(Fragment::new(format!("{ident} IS NOT NULL"))),
        FilterOp::IsTrue => return Ok(Fragment::new(if postgres { format!("{ident} = TRUE") } else { format!("{ident} <> 0") })),
        FilterOp::IsFalse => return Ok(Fragment::new(if postgres { format!("{ident} = FALSE") } else { format!("{ident} = 0") })),
        FilterOp::IsEmpty | FilterOp::IsNotEmpty => {
            let expr = compare_expr(ctx.driver, &ident, class, op);
            let comparison = if op == FilterOp::IsEmpty { "=" } else { "<>" };
            return Ok(Fragment::new(format!("{expr} {comparison} ''")));
        }
        _ => {}
    }
    let mut sql = Fragment::new(compare_expr(ctx.driver, &ident, class, op));
    let param = |sql: &mut Fragment, value: &EditValue| push_param(ctx, sql, class, value, id, column);
    match op {
        FilterOp::Eq | FilterOp::Neq | FilterOp::Gt | FilterOp::Gte | FilterOp::Lt | FilterOp::Lte => {
            sql.push_sql(match op {
                FilterOp::Eq => " = ",
                FilterOp::Neq => " <> ",
                FilterOp::Gt => " > ",
                FilterOp::Gte => " >= ",
                FilterOp::Lt => " < ",
                _ => " <= ",
            });
            param(&mut sql, &values[0])?;
        }
        FilterOp::Between | FilterOp::NotBetween => {
            sql.push_sql(if op == FilterOp::Between { " BETWEEN " } else { " NOT BETWEEN " });
            param(&mut sql, &values[0])?;
            sql.push_sql(" AND ");
            param(&mut sql, &values[1])?;
        }
        FilterOp::In | FilterOp::NotIn => {
            sql.push_sql(if op == FilterOp::In { " IN (" } else { " NOT IN (" });
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    sql.push_sql(", ");
                }
                param(&mut sql, value)?;
            }
            sql.push_sql(")");
        }
        _ => {
            let text = text_value(&values[0], id, column)?;
            let negated = op == FilterOp::NotContains;
            sql.push_sql(match (postgres, negated) {
                (true, false) => " ILIKE ",
                (true, true) => " NOT ILIKE ",
                (false, false) => " LIKE ",
                (false, true) => " NOT LIKE ",
            });
            sql.push_value(EditValue::Text(like_pattern(op, &text)));
            sql.push_sql(format!(" ESCAPE '{LIKE_ESCAPE}'"));
        }
    }
    Ok(sql)
}

/// The column as the comparison sees it. Casts make text operators work on enums and other types.
fn compare_expr(driver: Driver, ident: &str, class: ColumnClass, op: FilterOp) -> String {
    use FilterKind::*;
    match driver {
        Driver::Postgres if matches!(class.kind, Enum | Other | Json) => format!("{ident}::text"),
        Driver::Mysql if matches!(class.kind, Json | Other) && op.is_pattern() => format!("CAST({ident} AS CHAR)"),
        Driver::Sqlite => match class.kind {
            Date => format!("date({ident})"),
            Datetime => format!("datetime({ident})"),
            Time => format!("time({ident})"),
            Other if op.is_pattern() => format!("CAST({ident} AS TEXT)"),
            _ => ident.to_string(),
        },
        _ => ident.to_string(),
    }
}

fn like_pattern(op: FilterOp, text: &str) -> String {
    let mut escaped = String::with_capacity(text.len() + 2);
    for c in text.chars() {
        if matches!(c, '%' | '_') || c == LIKE_ESCAPE {
            escaped.push(LIKE_ESCAPE);
        }
        escaped.push(c);
    }
    match op {
        FilterOp::StartsWith => format!("{escaped}%"),
        FilterOp::EndsWith => format!("%{escaped}"),
        _ => format!("%{escaped}%"),
    }
}

fn scalar_text(value: &EditValue) -> Option<String> {
    match value {
        EditValue::Null => None,
        EditValue::Bool(value) => Some(value.to_string()),
        EditValue::Int(value) => Some(value.to_string()),
        EditValue::Float(value) => Some(value.to_string()),
        EditValue::Text(text) => Some(text.clone()),
    }
}

fn text_value(value: &EditValue, id: Option<&str>, column: &str) -> Result<String, FilterError> {
    let text = scalar_text(value)
        .ok_or_else(|| FilterError::new(id, format!("Use “is NULL” to find empty values in {column}.")))?;
    if text.chars().count() > MAX_VALUE_LEN {
        return Err(FilterError::new(id, format!("Values for {column} can be at most {MAX_VALUE_LEN} characters.")));
    }
    if text.contains('\0') {
        return Err(FilterError::new(id, format!("The value for {column} contains a NUL character.")));
    }
    Ok(text)
}

fn push_param(
    ctx: &FilterContext,
    sql: &mut Fragment,
    class: ColumnClass,
    value: &EditValue,
    id: Option<&str>,
    column: &str,
) -> Result<(), FilterError> {
    let postgres = ctx.driver == Driver::Postgres;
    let text = text_value(value, id, column)?;
    let invalid = |what: &str| FilterError::new(id, format!("“{}” is not a valid {what} for {column}.", text.trim()));
    match class.kind {
        FilterKind::Number => sql.push_number(number_text(value).ok_or_else(|| invalid("number"))?),
        FilterKind::Date => {
            let date = text.trim();
            if !is_date(date) {
                return Err(invalid("date (YYYY-MM-DD)"));
            }
            sql.push_value(EditValue::Text(date.to_string()));
            if postgres {
                sql.push_sql("::date");
            }
        }
        FilterKind::Datetime => {
            let mut stamp = normalize_datetime(&text).ok_or_else(|| invalid("date and time"))?;
            if postgres && class.tz {
                if let Some(offset) = ctx.utc_offset.and_then(offset_text) {
                    stamp.push_str(&offset);
                }
            }
            sql.push_value(EditValue::Text(stamp));
            if postgres {
                sql.push_sql(if class.tz { "::timestamptz" } else { "::timestamp" });
            }
        }
        FilterKind::Time => {
            sql.push_value(EditValue::Text(normalize_time(text.trim()).ok_or_else(|| invalid("time (HH:MM)"))?));
            if postgres {
                sql.push_sql("::time");
            }
        }
        FilterKind::Uuid => {
            let uuid = text.trim();
            if !is_uuid(uuid) {
                return Err(invalid("UUID"));
            }
            sql.push_value(EditValue::Text(uuid.to_ascii_lowercase()));
            if postgres {
                sql.push_sql("::uuid");
            }
        }
        FilterKind::Text | FilterKind::Enum | FilterKind::Other | FilterKind::Json => sql.push_value(EditValue::Text(text)),
        FilterKind::Boolean | FilterKind::Binary => {
            return Err(FilterError::new(id, format!("{column} can't be compared to a value.")));
        }
    }
    Ok(())
}

fn number_text(value: &EditValue) -> Option<String> {
    match value {
        EditValue::Int(value) => Some(value.to_string()),
        EditValue::Float(value) if value.is_finite() => Some(value.to_string()),
        EditValue::Text(text) => {
            let text = text.trim();
            is_number(text).then(|| text.trim_start_matches('+').to_string())
        }
        _ => None,
    }
}

fn is_number(text: &str) -> bool {
    if text.is_empty() || text.len() > 100 {
        return false;
    }
    let bytes = text.as_bytes();
    let mut i = usize::from(matches!(bytes[0], b'+' | b'-'));
    let digits_before = bytes[i..].iter().take_while(|b| b.is_ascii_digit()).count();
    i += digits_before;
    let mut digits_after = 0;
    if bytes.get(i) == Some(&b'.') {
        i += 1;
        digits_after = bytes[i..].iter().take_while(|b| b.is_ascii_digit()).count();
        i += digits_after;
    }
    if digits_before + digits_after == 0 {
        return false;
    }
    if matches!(bytes.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(bytes.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        let exponent = bytes[i..].iter().take_while(|b| b.is_ascii_digit()).count();
        if exponent == 0 {
            return false;
        }
        i += exponent;
    }
    i == bytes.len()
}

fn two_digits(text: &str) -> Option<u32> {
    (text.len() == 2 && text.bytes().all(|b| b.is_ascii_digit())).then(|| text.parse().ok()).flatten()
}

fn is_date(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' || !text.is_ascii() {
        return false;
    }
    let (Ok(year), Some(month), Some(day)) = (text[..4].parse::<u32>(), two_digits(&text[5..7]), two_digits(&text[8..])) else {
        return false;
    };
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&day)
}

/// `HH:MM`, `HH:MM:SS`, or `HH:MM:SS.ffffff`, written back with seconds.
fn normalize_time(text: &str) -> Option<String> {
    let (main, fraction) = match text.split_once('.') {
        Some((main, fraction)) => (main, Some(fraction)),
        None => (text, None),
    };
    let parts: Vec<&str> = main.split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let hour = two_digits(parts[0]).filter(|hour| *hour < 24)?;
    let minute = two_digits(parts[1]).filter(|minute| *minute < 60)?;
    let second = match parts.get(2) {
        Some(part) => two_digits(part).filter(|second| *second < 60)?,
        None => 0,
    };
    let mut out = format!("{hour:02}:{minute:02}:{second:02}");
    if let Some(fraction) = fraction {
        if fraction.is_empty() || fraction.len() > 6 || !fraction.bytes().all(|b| b.is_ascii_digit()) || parts.len() != 3 {
            return None;
        }
        out.push('.');
        out.push_str(fraction);
    }
    Some(out)
}

/// A date alone means midnight.
fn normalize_datetime(text: &str) -> Option<String> {
    let text = text.trim();
    if !text.is_ascii() {
        return None;
    }
    if is_date(text) {
        return Some(format!("{text} 00:00:00"));
    }
    if text.len() < 16 || !is_date(&text[..10]) || !matches!(text.as_bytes()[10], b' ' | b'T') {
        return None;
    }
    Some(format!("{} {}", &text[..10], normalize_time(&text[11..])?))
}

fn is_uuid(text: &str) -> bool {
    text.len() == 36
        && text.bytes().enumerate().all(|(index, b)| match index {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

fn offset_text(minutes: i32) -> Option<String> {
    if minutes.abs() > 14 * 60 {
        return None;
    }
    let sign = if minutes < 0 { '-' } else { '+' };
    Some(format!("{sign}{:02}:{:02}", minutes.abs() / 60, minutes.abs() % 60))
}

/** A search over a column's text, for suggesting existing values. */
pub fn suggestion_sql(driver: Driver, table: &str, column: &str, search: &str, sample: u32, limit: u32) -> Fragment {
    let ident = dialect(driver).quote_ident(column);
    let mut sql = Fragment::new(format!(
        "SELECT DISTINCT recon_value FROM (SELECT {ident} AS recon_value FROM {table} WHERE {ident} IS NOT NULL"
    ));
    let search = search.trim();
    if !search.is_empty() {
        let text = match driver {
            Driver::Postgres => format!("{ident}::text ILIKE "),
            Driver::Mysql => format!("CAST({ident} AS CHAR) LIKE "),
            Driver::Sqlite => format!("CAST({ident} AS TEXT) LIKE "),
        };
        sql.push_sql(format!(" AND {text}"));
        sql.push_value(EditValue::Text(like_pattern(FilterOp::Contains, search)));
        sql.push_sql(format!(" ESCAPE '{LIKE_ESCAPE}'"));
    }
    sql.push_sql(format!(" LIMIT {sample}) recon_sample ORDER BY recon_value LIMIT {limit}"));
    sql
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classes(driver: Driver, columns: &[(&str, &str)]) -> HashMap<String, ColumnClass> {
        columns.iter().map(|(name, data_type)| (name.to_string(), classify(driver, data_type))).collect()
    }

    fn parse(json: serde_json::Value) -> FilterNode {
        serde_json::from_value(json).unwrap()
    }

    fn sql(driver: Driver, columns: &HashMap<String, ColumnClass>, root: serde_json::Value) -> String {
        let ctx = FilterContext { driver, columns, utc_offset: Some(-240) };
        compile(&ctx, &parse(root)).unwrap().map(|sql| sql.inline(driver)).unwrap_or_default()
    }

    fn error(driver: Driver, columns: &HashMap<String, ColumnClass>, root: serde_json::Value) -> FilterError {
        let ctx = FilterContext { driver, columns, utc_offset: None };
        compile(&ctx, &parse(root)).unwrap_err()
    }

    #[test]
    fn classifies_native_types() {
        use FilterKind::*;
        let kind = |driver, data_type| classify(driver, data_type).kind;
        assert_eq!(kind(Driver::Mysql, "tinyint(1)"), Boolean);
        assert_eq!(kind(Driver::Mysql, "tinyint(4) unsigned"), Number);
        assert_eq!(kind(Driver::Mysql, "bigint unsigned zerofill"), Number);
        assert_eq!(kind(Driver::Mysql, "varchar(255)"), Text);
        assert_eq!(kind(Driver::Mysql, "enum('a','b')"), Enum);
        assert_eq!(kind(Driver::Mysql, "datetime(6)"), Datetime);
        assert_eq!(kind(Driver::Mysql, "binary(16)"), Binary);
        assert_eq!(kind(Driver::Postgres, "character varying(40)"), Text);
        assert_eq!(kind(Driver::Postgres, "numeric(10,2)"), Number);
        assert_eq!(kind(Driver::Postgres, "timestamp(3) without time zone"), Datetime);
        assert!(classify(Driver::Postgres, "timestamp with time zone").tz);
        assert_eq!(kind(Driver::Postgres, "time without time zone"), Time);
        assert_eq!(kind(Driver::Postgres, "integer[]"), Other);
        assert_eq!(kind(Driver::Postgres, "order_status"), Other);
        assert_eq!(kind(Driver::Sqlite, "INTEGER"), Number);
        assert_eq!(kind(Driver::Sqlite, "DATETIME"), Datetime);
        assert_eq!(kind(Driver::Sqlite, "date"), Date);
        assert_eq!(kind(Driver::Sqlite, "VARCHAR(10)"), Text);
        assert_eq!(kind(Driver::Sqlite, ""), Other);
    }

    #[test]
    fn reads_mysql_enum_labels() {
        assert_eq!(mysql_enum_values("enum('open','it''s','a\\\\b')"), vec!["open", "it's", "a\\b"]);
        assert!(mysql_enum_values("varchar(3)").is_empty());
    }

    #[test]
    fn builds_conditions_for_each_driver() {
        let root = serde_json::json!({
            "kind": "group", "match": "all", "children": [
                { "kind": "condition", "column": "status", "op": "eq", "values": ["Active"] },
                { "kind": "condition", "column": "amount", "op": "gt", "values": ["100.50"] },
                { "kind": "group", "match": "any", "children": [
                    { "kind": "condition", "column": "state", "op": "in", "values": ["FL", "GA"] },
                    { "kind": "condition", "column": "note", "op": "isNull" }
                ]}
            ]
        });
        let mysql = classes(Driver::Mysql, &[("status", "varchar(20)"), ("amount", "decimal(10,2)"), ("state", "char(2)"), ("note", "text")]);
        assert_eq!(
            sql(Driver::Mysql, &mysql, root.clone()),
            "`status` = 'Active' AND `amount` > 100.50 AND (`state` IN ('FL', 'GA') OR `note` IS NULL)"
        );
        let postgres = classes(Driver::Postgres, &[("status", "text"), ("amount", "numeric"), ("state", "character(2)"), ("note", "text")]);
        assert_eq!(
            sql(Driver::Postgres, &postgres, root),
            "\"status\" = E'Active' AND \"amount\" > 100.50 AND (\"state\" IN (E'FL', E'GA') OR \"note\" IS NULL)"
        );
    }

    #[test]
    fn escapes_values_and_like_patterns() {
        let columns = classes(Driver::Mysql, &[("name", "varchar(20)")]);
        let root = serde_json::json!({ "kind": "condition", "column": "name", "op": "contains", "values": ["50%_o'b\\!"] });
        assert_eq!(sql(Driver::Mysql, &columns, root.clone()), "`name` LIKE '%50!%!_o''b\\\\!!%' ESCAPE '!'");
        let columns = classes(Driver::Postgres, &[("name", "text")]);
        assert_eq!(sql(Driver::Postgres, &columns, root), "\"name\" ILIKE E'%50!%!_o''b\\\\!!%' ESCAPE '!'");
        let injection = serde_json::json!({ "kind": "condition", "column": "name", "op": "eq", "values": ["x' OR '1'='1"] });
        assert_eq!(sql(Driver::Postgres, &columns, injection), "\"name\" = E'x'' OR ''1''=''1'");
    }

    #[test]
    fn quotes_column_names_that_exist() {
        let columns = classes(Driver::Postgres, &[("we\"ird", "text")]);
        let root = serde_json::json!({ "kind": "condition", "column": "we\"ird", "op": "isNotNull" });
        assert_eq!(sql(Driver::Postgres, &columns, root), "\"we\"\"ird\" IS NOT NULL");
        let missing = serde_json::json!({ "kind": "condition", "id": "c1", "column": "id; DROP TABLE x", "op": "isNull" });
        let err = error(Driver::Postgres, &columns, missing);
        assert_eq!(err.id.as_deref(), Some("c1"));
        assert!(err.message.contains("has no column named"), "{}", err.message);
    }

    #[test]
    fn casts_postgres_values_by_kind() {
        let columns = classes(
            Driver::Postgres,
            &[("day", "date"), ("at", "timestamp with time zone"), ("local", "timestamp without time zone"), ("id", "uuid"), ("kind", "order_kind")],
        );
        let root = serde_json::json!({ "kind": "group", "match": "all", "children": [
            { "kind": "condition", "column": "day", "op": "between", "values": ["2026-01-01", "2026-01-31"] },
            { "kind": "condition", "column": "at", "op": "gte", "values": ["2026-09-27 00:00:00"] },
            { "kind": "condition", "column": "local", "op": "lt", "values": ["2026-09-27T10:30"] },
            { "kind": "condition", "column": "id", "op": "eq", "values": ["0F8FAD5B-D9CB-469F-A165-70867728950E"] },
            { "kind": "condition", "column": "kind", "op": "neq", "values": ["gift"] }
        ]});
        assert_eq!(
            sql(Driver::Postgres, &columns, root),
            "\"day\" BETWEEN E'2026-01-01'::date AND E'2026-01-31'::date \
             AND \"at\" >= E'2026-09-27 00:00:00-04:00'::timestamptz \
             AND \"local\" < E'2026-09-27 10:30:00'::timestamp \
             AND \"id\" = E'0f8fad5b-d9cb-469f-a165-70867728950e'::uuid \
             AND \"kind\"::text <> E'gift'"
        );
    }

    #[test]
    fn translates_booleans_per_driver() {
        let root = serde_json::json!({ "kind": "condition", "column": "active", "op": "isTrue" });
        let postgres = classes(Driver::Postgres, &[("active", "boolean")]);
        assert_eq!(sql(Driver::Postgres, &postgres, root.clone()), "\"active\" = TRUE");
        let mysql = classes(Driver::Mysql, &[("active", "tinyint(1)")]);
        assert_eq!(sql(Driver::Mysql, &mysql, root), "`active` <> 0");
    }

    #[test]
    fn builds_empty_string_conditions() {
        let empty = serde_json::json!({ "kind": "condition", "column": "note", "op": "isEmpty" });
        let not_empty = serde_json::json!({ "kind": "condition", "column": "note", "op": "isNotEmpty" });
        let mysql = classes(Driver::Mysql, &[("note", "varchar(20)")]);
        assert_eq!(sql(Driver::Mysql, &mysql, empty.clone()), "`note` = ''");
        assert_eq!(sql(Driver::Mysql, &mysql, not_empty), "`note` <> ''");
        let postgres = classes(Driver::Postgres, &[("note", "citext_array_like")]);
        assert_eq!(sql(Driver::Postgres, &postgres, empty), "\"note\"::text = ''");
        let numbers = classes(Driver::Mysql, &[("n", "int")]);
        let on_number = serde_json::json!({ "kind": "condition", "column": "n", "op": "isEmpty" });
        assert!(error(Driver::Mysql, &numbers, on_number).message.contains("can't be used"));
    }

    #[test]
    fn rejects_invalid_values_and_operators() {
        let columns = classes(Driver::Mysql, &[("amount", "int"), ("day", "date"), ("blob", "blob"), ("name", "text")]);
        let bad_number = serde_json::json!({ "kind": "condition", "id": "n", "column": "amount", "op": "gt", "values": ["12abc"] });
        assert!(error(Driver::Mysql, &columns, bad_number).message.contains("not a valid number"));
        let bad_date = serde_json::json!({ "kind": "condition", "column": "day", "op": "eq", "values": ["2026-02-30"] });
        assert!(error(Driver::Mysql, &columns, bad_date).message.contains("not a valid date"));
        let blob = serde_json::json!({ "kind": "condition", "column": "blob", "op": "eq", "values": ["x"] });
        assert!(error(Driver::Mysql, &columns, blob).message.contains("can't be used"));
        let empty_in = serde_json::json!({ "kind": "condition", "column": "name", "op": "in", "values": [] });
        assert!(error(Driver::Mysql, &columns, empty_in).message.contains("at least one"));
        let nul = serde_json::json!({ "kind": "condition", "column": "name", "op": "eq", "values": ["a\u{0}b"] });
        assert!(error(Driver::Mysql, &columns, nul).message.contains("NUL"));
        assert!(serde_json::from_value::<FilterNode>(serde_json::json!(
            { "kind": "condition", "column": "name", "op": "raw", "values": ["1=1"] }
        ))
        .is_err());
    }

    #[test]
    fn limits_size_and_depth() {
        let columns = classes(Driver::Sqlite, &[("n", "INTEGER")]);
        let many: Vec<_> = (0..=MAX_CONDITIONS)
            .map(|_| serde_json::json!({ "kind": "condition", "column": "n", "op": "isNull" }))
            .collect();
        let root = serde_json::json!({ "kind": "group", "match": "all", "children": many });
        assert!(error(Driver::Sqlite, &columns, root).message.contains("at most"));
        let mut deep = serde_json::json!({ "kind": "condition", "column": "n", "op": "isNull" });
        for _ in 0..MAX_DEPTH {
            deep = serde_json::json!({ "kind": "group", "match": "all", "children": [deep] });
        }
        assert!(error(Driver::Sqlite, &columns, deep).message.contains("nested"));
    }

    #[test]
    fn normalizes_dates_and_numbers() {
        assert!(is_number("-1.5e3") && is_number(".5") && is_number("+7") && !is_number("1e") && !is_number("."));
        assert_eq!(normalize_datetime("2026-09-27"), Some("2026-09-27 00:00:00".into()));
        assert_eq!(normalize_datetime("2026-09-27T08:05:09.25"), Some("2026-09-27 08:05:09.25".into()));
        assert_eq!(normalize_datetime("2026-09-27 24:00"), None);
        assert_eq!(normalize_time("7:05"), None);
        assert!(is_date("2024-02-29") && !is_date("2023-02-29"));
        assert_eq!(offset_text(330).as_deref(), Some("+05:30"));
    }

    #[test]
    fn binds_values_as_parameters() {
        let columns = classes(Driver::Sqlite, &[("n", "INTEGER"), ("name", "TEXT")]);
        let ctx = FilterContext { driver: Driver::Sqlite, columns: &columns, utc_offset: None };
        let root = parse(serde_json::json!({ "kind": "group", "match": "any", "children": [
            { "kind": "condition", "column": "n", "op": "between", "values": ["1", "2.5"] },
            { "kind": "condition", "column": "name", "op": "startsWith", "values": ["a"] }
        ]}));
        let (text, params) = compile(&ctx, &root).unwrap().unwrap().bound();
        assert_eq!(text, "\"n\" BETWEEN ? AND ? OR \"name\" LIKE ? ESCAPE '!'");
        assert_eq!(params, vec![EditValue::Int(1), EditValue::Float(2.5), EditValue::Text("a%".into())]);
    }
}
