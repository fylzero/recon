use std::fs;
use std::path::{Path, PathBuf};

use tauri::{AppHandle, Manager};

use crate::models::AppData;

const SETTINGS_FILE: &str = "settings.json";
const HISTORY_FILE: &str = "query-history.jsonl";

fn app_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|err| format!("Could not resolve the app data directory: {err}"))?;
    fs::create_dir_all(&dir).map_err(|err| format!("Could not create the app data directory: {err}"))?;
    Ok(dir)
}

pub fn data_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app_dir(app)?.join(SETTINGS_FILE))
}

pub fn history_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app_dir(app)?.join(HISTORY_FILE))
}

fn read_app_data(path: &Path) -> Result<AppData, String> {
    let raw = fs::read_to_string(path).map_err(|err| format!("Could not read {SETTINGS_FILE}: {err}"))?;
    serde_json::from_str(&raw).map_err(|err| format!("Could not parse {SETTINGS_FILE}: {err}"))
}

pub fn load(app: &AppHandle) -> Result<AppData, String> {
    let path = data_path(app)?;
    if path.exists() {
        return read_app_data(&path);
    }
    Ok(AppData::default())
}

pub fn save(app: &AppHandle, data: &AppData) -> Result<(), String> {
    let path = data_path(app)?;
    let raw = serde_json::to_string_pretty(data)
        .map_err(|err| format!("Could not serialize {SETTINGS_FILE}: {err}"))?;
    fs::write(&path, raw).map_err(|err| format!("Could not write {SETTINGS_FILE}: {err}"))
}
