mod commands;
mod db;
mod db_commands;
mod menu;
mod models;
mod persist;
mod query_log;
mod secrets;
mod window_state;

use commands::AppState;
use db::{ResultStore, SessionStore};
use std::sync::Mutex;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .menu(|handle| menu::build(handle))
        .on_menu_event(|app, event| {
            menu::handle_event(app, event.id().as_ref());
        })
        .setup(|app| {
            let handle = app.handle().clone();
            let data = persist::load(&handle).unwrap_or_default();
            let bounds = data.window.clone();
            if let Ok(path) = persist::history_path(&handle) {
                query_log::init(path, Some(handle.clone()));
            }
            app.manage(AppState {
                data: Mutex::new(data),
            });
            app.manage(SessionStore::default());
            app.manage(ResultStore::default());
            if let (Some(window), Some(bounds)) = (app.get_webview_window("main"), bounds) {
                window_state::apply(&window, &bounds);
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            window_state::handle_event(window, event);
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_state,
            commands::create_group,
            commands::update_group,
            commands::delete_group,
            commands::toggle_group,
            commands::set_all_groups_expanded,
            commands::reorder_groups,
            commands::save_connection,
            commands::remove_connection,
            commands::reorder_connections,
            commands::has_saved_password,
            commands::update_preferences,
            commands::replace_app_data,
            commands::write_text_file,
            commands::read_text_file,
            commands::settings_file_path,
            commands::reveal_settings_file,
            commands::reveal_path,
            commands::query_history,
            commands::query_history_paused,
            commands::set_query_history_paused,
            commands::clear_query_history,
            window_state::get_window_state,
            window_state::update_window_state,
            db_commands::test_connection,
            db_commands::create_sqlite_database,
            db_commands::connect,
            db_commands::disconnect,
            db_commands::list_databases,
            db_commands::set_database,
            db_commands::list_tables,
            db_commands::table_structure,
            db_commands::schema_columns,
            db_commands::browse_table,
            db_commands::run_query,
            db_commands::fetch_rows,
            db_commands::close_results,
            db_commands::cancel_query,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
