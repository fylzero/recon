use serde::{Deserialize, Serialize};

fn default_code_font() -> String {
    "jetbrains".into()
}

fn default_editor_font_size() -> f64 {
    13.0
}

fn default_grid_font_size() -> f64 {
    12.0
}

fn default_page_size() -> u32 {
    300
}

fn default_query_row_limit() -> u32 {
    10_000
}

fn default_sidebar_width() -> u32 {
    260
}

fn default_header_color() -> String {
    "#16323c".into()
}

fn default_ssl_mode() -> String {
    "prefer".into()
}

fn default_true() -> bool {
    true
}

pub const DEFAULT_EDITOR_FONT_SIZE: f64 = 13.0;
pub const DEFAULT_GRID_FONT_SIZE: f64 = 12.0;
pub const PAGE_SIZE_MIN: u32 = 50;
pub const PAGE_SIZE_MAX: u32 = 5_000;
pub const QUERY_ROW_LIMIT_MIN: u32 = 100;
pub const QUERY_ROW_LIMIT_MAX: u32 = 200_000;
pub const SIDEBAR_WIDTH_MIN: u32 = 180;
pub const SIDEBAR_WIDTH_MAX: u32 = 560;

pub fn sanitize_font_family(value: &str) -> String {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 80
        || value
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\' | ';' | '{' | '}'))
    {
        return default_code_font();
    }
    match value.to_ascii_lowercase().as_str() {
        "jetbrains" | "jetbrains mono" => "jetbrains".into(),
        "system" | "system mono" | "default" | "ui-monospace" => "system".into(),
        "sf-mono" | "sf mono" | "sfmono" => "sf-mono".into(),
        "menlo" => "menlo".into(),
        "monaco" => "monaco".into(),
        "courier" | "courier new" => "courier".into(),
        _ => value.replace(['\'', '"'], ""),
    }
}

pub fn sanitize_font_size(value: f64, default: f64) -> f64 {
    if !value.is_finite() {
        return default;
    }
    let clamped = value.clamp(9.0, 22.0);
    (clamped * 2.0).round() / 2.0
}

pub fn sanitize_color(color: &str) -> Option<String> {
    let color = color.trim();
    let hex = color.strip_prefix('#')?;
    if (hex.len() == 6 || hex.len() == 3) && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(color.to_string())
    } else {
        None
    }
}

pub const MIN_WINDOW_WIDTH: u32 = 960;
pub const MIN_WINDOW_HEIGHT: u32 = 640;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowState {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub maximized: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Driver {
    Mysql,
    Postgres,
    Sqlite,
}

impl Driver {
    pub fn default_port(self) -> u16 {
        match self {
            Driver::Mysql => 3306,
            Driver::Postgres => 5432,
            Driver::Sqlite => 0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Driver::Mysql => "MySQL",
            Driver::Postgres => "PostgreSQL",
            Driver::Sqlite => "SQLite",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionEntry {
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub driver: Driver,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub database: String,
    #[serde(default)]
    pub file_path: String,
    #[serde(default = "default_ssl_mode")]
    pub ssl_mode: String,
    #[serde(default)]
    pub header_color: String,
    #[serde(default = "default_true")]
    pub save_password: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionGroup {
    pub id: String,
    pub name: String,
    #[serde(default = "default_true")]
    pub expanded: bool,
    #[serde(default = "default_header_color")]
    pub header_color: String,
    #[serde(default)]
    pub connections: Vec<ConnectionEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppData {
    #[serde(default)]
    pub groups: Vec<ConnectionGroup>,
    #[serde(default)]
    pub connections: Vec<ConnectionEntry>,
    #[serde(default = "default_code_font")]
    pub editor_font_family: String,
    #[serde(default = "default_editor_font_size")]
    pub editor_font_size: f64,
    #[serde(default = "default_code_font")]
    pub grid_font_family: String,
    #[serde(default = "default_grid_font_size")]
    pub grid_font_size: f64,
    #[serde(default = "default_page_size")]
    pub page_size: u32,
    #[serde(default = "default_query_row_limit")]
    pub query_row_limit: u32,
    #[serde(default = "default_sidebar_width")]
    pub sidebar_width: u32,
    #[serde(default)]
    pub window: Option<WindowState>,
}

impl Default for AppData {
    fn default() -> Self {
        Self {
            groups: Vec::new(),
            connections: Vec::new(),
            editor_font_family: default_code_font(),
            editor_font_size: default_editor_font_size(),
            grid_font_family: default_code_font(),
            grid_font_size: default_grid_font_size(),
            page_size: default_page_size(),
            query_row_limit: default_query_row_limit(),
            sidebar_width: default_sidebar_width(),
            window: None,
        }
    }
}

impl AppData {
    pub fn find_connection(&self, connection_id: &str) -> Option<&ConnectionEntry> {
        self.connections
            .iter()
            .chain(self.groups.iter().flat_map(|group| group.connections.iter()))
            .find(|entry| entry.id == connection_id)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreferencesPatch {
    pub editor_font_family: Option<String>,
    pub editor_font_size: Option<f64>,
    pub grid_font_family: Option<String>,
    pub grid_font_size: Option<f64>,
    pub page_size: Option<u32>,
    pub query_row_limit: Option<u32>,
    pub sidebar_width: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_connections_as_camel_case_json() {
        let mut data = AppData::default();
        data.groups.push(ConnectionGroup {
            id: "g1".into(),
            name: "Work".into(),
            expanded: true,
            header_color: "#16323c".into(),
            connections: vec![ConnectionEntry {
                id: "c1".into(),
                name: "Local".into(),
                driver: Driver::Postgres,
                host: "127.0.0.1".into(),
                port: 5432,
                user: "postgres".into(),
                database: "app".into(),
                file_path: String::new(),
                ssl_mode: "prefer".into(),
                header_color: String::new(),
                save_password: true,
            }],
        });
        let json = serde_json::to_string(&data).unwrap();
        assert!(json.contains("\"driver\":\"postgres\""));
        assert!(json.contains("\"savePassword\":true"));
        assert!(!json.to_lowercase().contains("password\":\""));
        let parsed: AppData = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.groups[0].connections[0].port, 5432);
        assert!(parsed.find_connection("c1").is_some());
    }

    #[test]
    fn missing_fields_use_defaults() {
        let parsed: AppData = serde_json::from_str(r#"{"groups":[]}"#).unwrap();
        assert_eq!(parsed.page_size, 300);
        assert_eq!(parsed.query_row_limit, 10_000);
        assert_eq!(parsed.editor_font_family, "jetbrains");
        assert_eq!(parsed.grid_font_size, 12.0);
    }

    #[test]
    fn sanitizes_font_family_aliases_and_rejects_paths() {
        assert_eq!(sanitize_font_family("JetBrains Mono"), "jetbrains");
        assert_eq!(sanitize_font_family("SF Mono"), "sf-mono");
        assert_eq!(sanitize_font_family("Fira Code"), "Fira Code");
        assert_eq!(sanitize_font_family(""), "jetbrains");
        assert_eq!(sanitize_font_family("/System/Library/Fonts/Menlo.ttc"), "jetbrains");
    }

    #[test]
    fn sanitizes_colors() {
        assert_eq!(sanitize_color("#abc"), Some("#abc".into()));
        assert_eq!(sanitize_color(" #16323c "), Some("#16323c".into()));
        assert_eq!(sanitize_color("#zzzzzz"), None);
        assert_eq!(sanitize_color("red"), None);
    }
}
