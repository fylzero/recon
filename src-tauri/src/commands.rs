use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use tauri::{AppHandle, State};

use crate::models::{
    sanitize_color, sanitize_font_family, sanitize_font_size, sanitize_list_font_family, AppData,
    ConnectionEntry, ConnectionGroup, Driver, PreferencesPatch, SshAuth, SshTunnel,
    DEFAULT_EDITOR_FONT_SIZE, DEFAULT_GRID_FONT_SIZE, DEFAULT_LIST_FONT_SIZE, DEFAULT_SSH_PORT,
    MAX_AUTO_COLUMN_WIDTH_MAX, MAX_AUTO_COLUMN_WIDTH_MIN, PAGE_SIZE_MAX, PAGE_SIZE_MIN,
    QUERY_ROW_LIMIT_MAX, QUERY_ROW_LIMIT_MIN, SIDEBAR_WIDTH_MAX, SIDEBAR_WIDTH_MIN,
};
use crate::{persist, query_log, secrets};

const DEFAULT_GROUP_COLOR: &str = "#16323c";
const SSL_MODES: [&str; 3] = ["disable", "prefer", "require"];

pub struct AppState {
    pub data: Mutex<AppData>,
}

fn lock(state: &AppState) -> Result<std::sync::MutexGuard<'_, AppData>, String> {
    state.data.lock().map_err(|err| err.to_string())
}

fn find_group_mut<'a>(
    data: &'a mut AppData,
    group_id: &str,
) -> Result<&'a mut ConnectionGroup, String> {
    data.groups
        .iter_mut()
        .find(|group| group.id == group_id)
        .ok_or_else(|| "Group not found".to_string())
}

fn connection_list_mut<'a>(
    data: &'a mut AppData,
    group_id: Option<&str>,
) -> Result<&'a mut Vec<ConnectionEntry>, String> {
    match group_id {
        None => Ok(&mut data.connections),
        Some(id) => Ok(&mut find_group_mut(data, id)?.connections),
    }
}

fn take_connection(data: &mut AppData, connection_id: &str) -> Option<ConnectionEntry> {
    if let Some(index) = data.connections.iter().position(|entry| entry.id == connection_id) {
        return Some(data.connections.remove(index));
    }
    for group in &mut data.groups {
        if let Some(index) = group
            .connections
            .iter()
            .position(|entry| entry.id == connection_id)
        {
            return Some(group.connections.remove(index));
        }
    }
    None
}

fn connection_location(data: &AppData, connection_id: &str) -> Option<(Option<String>, usize)> {
    if let Some(index) = data.connections.iter().position(|entry| entry.id == connection_id) {
        return Some((None, index));
    }
    data.groups.iter().find_map(|group| {
        group
            .connections
            .iter()
            .position(|entry| entry.id == connection_id)
            .map(|index| (Some(group.id.clone()), index))
    })
}

fn sanitize_ssh(ssh: &mut SshTunnel) -> Result<(), String> {
    ssh.host = ssh.host.trim().to_string();
    ssh.user = ssh.user.trim().to_string();
    ssh.key_path = ssh.key_path.trim().to_string();
    if ssh.port == 0 {
        ssh.port = DEFAULT_SSH_PORT;
    }
    if !ssh.enabled {
        return Ok(());
    }
    if ssh.host.is_empty() {
        return Err("An SSH host is required.".into());
    }
    if ssh.user.is_empty() {
        return Err("An SSH user name is required.".into());
    }
    if ssh.auth == SshAuth::Key && ssh.key_path.is_empty() {
        return Err("Choose an SSH private key file.".into());
    }
    Ok(())
}

pub fn sanitize_connection(mut entry: ConnectionEntry) -> Result<ConnectionEntry, String> {
    entry.name = entry.name.trim().to_string();
    entry.host = entry.host.trim().to_string();
    entry.user = entry.user.trim().to_string();
    entry.database = entry.database.trim().to_string();
    entry.file_path = entry.file_path.trim().to_string();
    entry.header_color = sanitize_color(&entry.header_color).unwrap_or_default();
    let ssl = entry.ssl_mode.trim().to_ascii_lowercase();
    entry.ssl_mode = if SSL_MODES.contains(&ssl.as_str()) {
        ssl
    } else {
        "prefer".into()
    };
    match entry.driver {
        Driver::Sqlite => {
            if entry.file_path.is_empty() {
                return Err("Choose a SQLite database file.".into());
            }
            entry.host.clear();
            entry.port = 0;
            entry.user.clear();
            entry.database.clear();
            entry.save_password = false;
            entry.ssh = SshTunnel::default();
            if entry.name.is_empty() {
                entry.name = Path::new(&entry.file_path)
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "SQLite".into());
            }
        }
        Driver::Mysql | Driver::Postgres => {
            if entry.host.is_empty() {
                entry.host = "127.0.0.1".into();
            }
            if entry.port == 0 {
                entry.port = entry.driver.default_port();
            }
            if entry.user.is_empty() {
                return Err("A user name is required.".into());
            }
            entry.file_path.clear();
            sanitize_ssh(&mut entry.ssh)?;
            if entry.name.is_empty() {
                entry.name = if entry.database.is_empty() {
                    entry.host.clone()
                } else {
                    format!("{}/{}", entry.host, entry.database)
                };
            }
        }
    }
    Ok(entry)
}

fn sanitize_app_data(mut data: AppData) -> Result<AppData, String> {
    data.editor_font_family = sanitize_font_family(&data.editor_font_family);
    data.editor_font_size = sanitize_font_size(data.editor_font_size, DEFAULT_EDITOR_FONT_SIZE);
    data.grid_font_family = sanitize_font_family(&data.grid_font_family);
    data.grid_font_size = sanitize_font_size(data.grid_font_size, DEFAULT_GRID_FONT_SIZE);
    data.list_font_family = sanitize_list_font_family(&data.list_font_family);
    data.list_font_size = sanitize_font_size(data.list_font_size, DEFAULT_LIST_FONT_SIZE);
    data.page_size = data.page_size.clamp(PAGE_SIZE_MIN, PAGE_SIZE_MAX);
    data.query_row_limit = data
        .query_row_limit
        .clamp(QUERY_ROW_LIMIT_MIN, QUERY_ROW_LIMIT_MAX);
    data.sidebar_width = data.sidebar_width.clamp(SIDEBAR_WIDTH_MIN, SIDEBAR_WIDTH_MAX);
    data.max_auto_column_width = data
        .max_auto_column_width
        .clamp(MAX_AUTO_COLUMN_WIDTH_MIN, MAX_AUTO_COLUMN_WIDTH_MAX);
    if let Some(window) = &mut data.window {
        window.width = window.width.max(crate::models::MIN_WINDOW_WIDTH);
        window.height = window.height.max(crate::models::MIN_WINDOW_HEIGHT);
    }
    let mut connections = Vec::with_capacity(data.connections.len());
    for mut entry in data.connections {
        if entry.id.trim().is_empty() {
            entry.id = uuid::Uuid::new_v4().to_string();
        }
        connections.push(sanitize_connection(entry)?);
    }
    data.connections = connections;
    for group in &mut data.groups {
        if group.id.trim().is_empty() {
            group.id = uuid::Uuid::new_v4().to_string();
        }
        group.name = group.name.trim().to_string();
        if group.name.is_empty() {
            return Err("Every group needs a name".into());
        }
        group.header_color =
            sanitize_color(&group.header_color).unwrap_or_else(|| DEFAULT_GROUP_COLOR.into());
        let mut next = Vec::with_capacity(group.connections.len());
        for mut entry in group.connections.drain(..) {
            if entry.id.trim().is_empty() {
                entry.id = uuid::Uuid::new_v4().to_string();
            }
            next.push(sanitize_connection(entry)?);
        }
        group.connections = next;
    }
    Ok(data)
}

fn reorder_by_ids<T>(
    items: &mut Vec<T>,
    ids: Vec<String>,
    id_of: impl Fn(&T) -> &str,
    label: &str,
) -> Result<(), String> {
    if ids.len() != items.len() {
        return Err(format!("{label} list does not match saved {label}s."));
    }
    let mut by_id: HashMap<String, T> = items
        .drain(..)
        .map(|item| (id_of(&item).to_string(), item))
        .collect();
    let mut next = Vec::with_capacity(ids.len());
    for id in ids {
        let item = by_id
            .remove(&id)
            .ok_or_else(|| format!("{label} not found"))?;
        next.push(item);
    }
    *items = next;
    Ok(())
}

#[tauri::command]
pub fn get_state(state: State<AppState>) -> Result<AppData, String> {
    Ok(lock(&state)?.clone())
}

#[tauri::command]
pub fn create_group(
    app: AppHandle,
    state: State<AppState>,
    name: String,
    header_color: Option<String>,
) -> Result<ConnectionGroup, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("Group name is required".into());
    }
    let group = ConnectionGroup {
        id: uuid::Uuid::new_v4().to_string(),
        name,
        expanded: true,
        header_color: header_color
            .as_deref()
            .and_then(sanitize_color)
            .unwrap_or_else(|| DEFAULT_GROUP_COLOR.into()),
        connections: Vec::new(),
    };
    let mut data = lock(&state)?;
    data.groups.insert(0, group.clone());
    persist::save(&app, &data)?;
    Ok(group)
}

#[tauri::command]
pub fn update_group(
    app: AppHandle,
    state: State<AppState>,
    group_id: String,
    name: String,
    header_color: Option<String>,
) -> Result<(), String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("Group name is required".into());
    }
    let mut data = lock(&state)?;
    let group = find_group_mut(&mut data, &group_id)?;
    group.name = name;
    if let Some(color) = header_color.as_deref().and_then(sanitize_color) {
        group.header_color = color;
    }
    persist::save(&app, &data)
}

#[tauri::command]
pub fn delete_group(app: AppHandle, state: State<AppState>, group_id: String) -> Result<(), String> {
    let mut data = lock(&state)?;
    let index = data
        .groups
        .iter()
        .position(|group| group.id == group_id)
        .ok_or_else(|| "Group not found".to_string())?;
    let group = data.groups.remove(index);
    persist::save(&app, &data)?;
    drop(data);
    for entry in group.connections {
        let _ = secrets::delete_all(&entry.id);
    }
    Ok(())
}

#[tauri::command]
pub fn toggle_group(app: AppHandle, state: State<AppState>, group_id: String) -> Result<bool, String> {
    let mut data = lock(&state)?;
    let group = find_group_mut(&mut data, &group_id)?;
    group.expanded = !group.expanded;
    let expanded = group.expanded;
    persist::save(&app, &data)?;
    Ok(expanded)
}

#[tauri::command]
pub fn set_all_groups_expanded(
    app: AppHandle,
    state: State<AppState>,
    expanded: bool,
) -> Result<(), String> {
    let mut data = lock(&state)?;
    for group in &mut data.groups {
        group.expanded = expanded;
    }
    persist::save(&app, &data)
}

#[tauri::command]
pub fn reorder_groups(
    app: AppHandle,
    state: State<AppState>,
    group_ids: Vec<String>,
) -> Result<(), String> {
    let mut data = lock(&state)?;
    reorder_by_ids(&mut data.groups, group_ids, |group| &group.id, "Group")?;
    persist::save(&app, &data)
}

#[tauri::command]
pub fn save_connection(
    app: AppHandle,
    state: State<AppState>,
    group_id: Option<String>,
    connection: ConnectionEntry,
    password: Option<String>,
    ssh_secret: Option<String>,
) -> Result<ConnectionEntry, String> {
    let mut entry = sanitize_connection(connection)?;
    let is_new = entry.id.trim().is_empty();
    if is_new {
        entry.id = uuid::Uuid::new_v4().to_string();
    }
    {
        let mut data = lock(&state)?;
        let location = connection_location(&data, &entry.id);
        if !is_new && location.is_none() {
            return Err("Connection not found".into());
        }
        connection_list_mut(&mut data, group_id.as_deref())?;
        match location {
            Some((current_group, index)) if current_group == group_id => {
                connection_list_mut(&mut data, group_id.as_deref())?[index] = entry.clone();
            }
            _ => {
                take_connection(&mut data, &entry.id);
                connection_list_mut(&mut data, group_id.as_deref())?.push(entry.clone());
            }
        }
        persist::save(&app, &data)?;
    }
    if !entry.save_password {
        secrets::delete(&entry.id)?;
    } else if let Some(password) = password {
        if password.is_empty() {
            secrets::delete(&entry.id)?;
        } else {
            secrets::set(&entry.id, &password)?;
        }
    }
    let ssh_account = secrets::ssh_account(&entry.id);
    if !entry.ssh.uses_secret() {
        secrets::delete(&ssh_account)?;
    } else if let Some(secret) = ssh_secret.filter(|value| !value.is_empty()) {
        secrets::set(&ssh_account, &secret)?;
    }
    Ok(entry)
}

#[tauri::command]
pub fn remove_connection(
    app: AppHandle,
    state: State<AppState>,
    connection_id: String,
) -> Result<(), String> {
    {
        let mut data = lock(&state)?;
        take_connection(&mut data, &connection_id)
            .ok_or_else(|| "Connection not found".to_string())?;
        persist::save(&app, &data)?;
    }
    secrets::delete_all(&connection_id)
}

#[tauri::command]
pub fn reorder_connections(
    app: AppHandle,
    state: State<AppState>,
    group_id: Option<String>,
    connection_ids: Vec<String>,
) -> Result<(), String> {
    let mut data = lock(&state)?;
    let list = connection_list_mut(&mut data, group_id.as_deref())?;
    reorder_by_ids(list, connection_ids, |entry| &entry.id, "Connection")?;
    persist::save(&app, &data)
}

#[tauri::command]
pub fn has_saved_password(connection_id: String) -> Result<bool, String> {
    Ok(secrets::get(&connection_id)?.is_some())
}

#[tauri::command]
pub fn list_ssh_keys() -> Vec<String> {
    crate::db::ssh::find_private_keys()
}

#[tauri::command]
pub fn has_saved_ssh_secret(connection_id: String) -> Result<bool, String> {
    Ok(secrets::get(&secrets::ssh_account(&connection_id))?.is_some())
}

#[tauri::command]
pub fn update_preferences(
    app: AppHandle,
    state: State<AppState>,
    patch: PreferencesPatch,
) -> Result<AppData, String> {
    let mut data = lock(&state)?;
    if let Some(value) = patch.editor_font_family {
        data.editor_font_family = sanitize_font_family(&value);
    }
    if let Some(value) = patch.editor_font_size {
        data.editor_font_size = sanitize_font_size(value, DEFAULT_EDITOR_FONT_SIZE);
    }
    if let Some(value) = patch.grid_font_family {
        data.grid_font_family = sanitize_font_family(&value);
    }
    if let Some(value) = patch.grid_font_size {
        data.grid_font_size = sanitize_font_size(value, DEFAULT_GRID_FONT_SIZE);
    }
    if let Some(value) = patch.list_font_family {
        data.list_font_family = sanitize_list_font_family(&value);
    }
    if let Some(value) = patch.list_font_size {
        data.list_font_size = sanitize_font_size(value, DEFAULT_LIST_FONT_SIZE);
    }
    if let Some(value) = patch.page_size {
        data.page_size = value.clamp(PAGE_SIZE_MIN, PAGE_SIZE_MAX);
    }
    if let Some(value) = patch.query_row_limit {
        data.query_row_limit = value.clamp(QUERY_ROW_LIMIT_MIN, QUERY_ROW_LIMIT_MAX);
    }
    if let Some(value) = patch.sidebar_width {
        data.sidebar_width = value.clamp(SIDEBAR_WIDTH_MIN, SIDEBAR_WIDTH_MAX);
    }
    if let Some(value) = patch.max_auto_column_width {
        data.max_auto_column_width =
            value.clamp(MAX_AUTO_COLUMN_WIDTH_MIN, MAX_AUTO_COLUMN_WIDTH_MAX);
    }
    persist::save(&app, &data)?;
    Ok(data.clone())
}

#[tauri::command]
pub fn replace_app_data(
    app: AppHandle,
    state: State<AppState>,
    data: AppData,
) -> Result<AppData, String> {
    let sanitized = sanitize_app_data(data)?;
    let mut lock = lock(&state)?;
    *lock = sanitized.clone();
    persist::save(&app, &lock)?;
    Ok(sanitized)
}

#[tauri::command]
pub fn write_text_file(path: String, contents: String) -> Result<(), String> {
    if path.trim().is_empty() {
        return Err("Choose a file to export.".into());
    }
    if let Some(parent) = Path::new(&path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("Could not create the export folder: {err}"))?;
        }
    }
    std::fs::write(&path, contents).map_err(|err| format!("Could not write the file: {err}"))
}

#[tauri::command]
pub fn read_text_file(path: String) -> Result<String, String> {
    if path.trim().is_empty() {
        return Err("Choose a file to import.".into());
    }
    std::fs::read_to_string(&path).map_err(|err| format!("Could not read the file: {err}"))
}

#[tauri::command]
pub fn settings_file_path(app: AppHandle) -> Result<String, String> {
    persist::data_path(&app)?
        .to_str()
        .map(str::to_string)
        .ok_or_else(|| "Settings path is not valid UTF-8".into())
}

#[tauri::command]
pub fn reveal_settings_file(app: AppHandle) -> Result<(), String> {
    let path = persist::data_path(&app)?;
    if !path.exists() {
        persist::save(&app, &AppData::default())?;
    }
    reveal_in_finder(&path, "settings file")
}

#[tauri::command]
pub fn reveal_path(path: String) -> Result<(), String> {
    reveal_in_finder(Path::new(&path), "file")
}

fn reveal_in_finder(path: &Path, label: &str) -> Result<(), String> {
    let status = std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .status()
        .map_err(|err| format!("Could not reveal the {label}: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("Could not reveal the {label}."))
    }
}

#[tauri::command]
pub fn query_history() -> Vec<query_log::QueryLogEntry> {
    query_log::list()
}

#[tauri::command]
pub fn query_history_paused() -> bool {
    query_log::paused()
}

#[tauri::command]
pub fn set_query_history_paused(paused: bool) -> Result<(), String> {
    query_log::set_paused(paused)
}

#[tauri::command]
pub fn clear_query_history() -> Result<(), String> {
    query_log::clear()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(driver: Driver) -> ConnectionEntry {
        ConnectionEntry {
            id: String::new(),
            name: String::new(),
            driver,
            host: String::new(),
            port: 0,
            user: "root".into(),
            database: String::new(),
            file_path: String::new(),
            ssl_mode: "bogus".into(),
            header_color: "nope".into(),
            save_password: true,
            ssh: SshTunnel::default(),
        }
    }

    #[test]
    fn validates_ssh_tunnels() {
        let mut mysql = entry(Driver::Mysql);
        mysql.ssh.enabled = true;
        assert!(sanitize_connection(mysql.clone()).is_err());
        mysql.ssh.host = " bastion.example.com ".into();
        mysql.ssh.user = "deploy".into();
        mysql.ssh.port = 0;
        let saved = sanitize_connection(mysql.clone()).unwrap();
        assert_eq!(saved.ssh.host, "bastion.example.com");
        assert_eq!(saved.ssh.port, 22);
        mysql.ssh.auth = SshAuth::Key;
        assert!(sanitize_connection(mysql.clone()).is_err());
        mysql.ssh.key_path = "~/.ssh/id_ed25519".into();
        assert!(sanitize_connection(mysql).is_ok());

        let mut lite = entry(Driver::Sqlite);
        lite.file_path = "/tmp/app.db".into();
        lite.ssh.enabled = true;
        assert!(!sanitize_connection(lite).unwrap().ssh.enabled);
    }

    #[test]
    fn server_connections_get_defaults() {
        let mysql = sanitize_connection(entry(Driver::Mysql)).unwrap();
        assert_eq!(mysql.host, "127.0.0.1");
        assert_eq!(mysql.port, 3306);
        assert_eq!(mysql.ssl_mode, "prefer");
        assert_eq!(mysql.header_color, "");
        assert_eq!(mysql.name, "127.0.0.1");

        let mut pg = entry(Driver::Postgres);
        pg.database = "app".into();
        let pg = sanitize_connection(pg).unwrap();
        assert_eq!(pg.port, 5432);
        assert_eq!(pg.name, "127.0.0.1/app");
    }

    #[test]
    fn server_connections_require_user() {
        let mut mysql = entry(Driver::Mysql);
        mysql.user = " ".into();
        assert!(sanitize_connection(mysql).is_err());
    }

    #[test]
    fn sqlite_requires_file_and_drops_server_fields() {
        assert!(sanitize_connection(entry(Driver::Sqlite)).is_err());
        let mut lite = entry(Driver::Sqlite);
        lite.file_path = "/tmp/app.db".into();
        let lite = sanitize_connection(lite).unwrap();
        assert_eq!(lite.name, "app");
        assert!(lite.user.is_empty());
        assert!(!lite.save_password);
    }

    #[test]
    fn moves_connections_between_groups() {
        let mut data = AppData::default();
        data.groups.push(ConnectionGroup {
            id: "g1".into(),
            name: "Work".into(),
            expanded: true,
            header_color: DEFAULT_GROUP_COLOR.into(),
            connections: Vec::new(),
        });
        let mut conn = sanitize_connection(entry(Driver::Mysql)).unwrap();
        conn.id = "c1".into();
        data.connections.push(conn);
        assert_eq!(connection_location(&data, "c1"), Some((None, 0)));
        let taken = take_connection(&mut data, "c1").unwrap();
        find_group_mut(&mut data, "g1").unwrap().connections.push(taken);
        assert_eq!(connection_location(&data, "c1"), Some((Some("g1".into()), 0)));
        assert!(data.connections.is_empty());
    }
}
