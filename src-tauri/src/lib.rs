pub mod askpass;
mod commands;
mod db;
mod db_commands;
mod menu;
mod models;
mod persist;
mod prompts;
mod query_log;
mod secrets;
mod tabular_commands;
mod transfer_commands;
mod window_state;

use commands::AppState;
use db::{ResultStore, SessionStore};
use std::sync::Mutex;
use tauri::Manager;
use transfer_commands::TransferStore;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
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
            app.manage(TransferStore::default());
            app.manage(prompts::PromptStore::default());
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
            commands::reorder_dashboard,
            commands::save_connection,
            commands::remove_connection,
            commands::reorder_connections,
            commands::move_connection,
            commands::save_query,
            commands::delete_saved_query,
            commands::has_saved_password,
            commands::has_saved_ssh_secret,
            commands::has_saved_ssh_password,
            commands::list_ssh_keys,
            commands::list_ssh_hosts,
            commands::resolve_ssh_host,
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
            prompts::answer_prompt,
            db_commands::test_connection,
            db_commands::create_sqlite_database,
            db_commands::connect,
            db_commands::reconnect,
            db_commands::disconnect,
            db_commands::list_databases,
            db_commands::set_database,
            db_commands::create_database,
            db_commands::drop_database,
            db_commands::rename_database,
            db_commands::list_tables,
            db_commands::truncate_tables,
            db_commands::drop_tables,
            db_commands::table_structure,
            db_commands::schema_columns,
            db_commands::schema_diagram,
            db_commands::morph_types,
            db_commands::browse_table,
            db_commands::count_rows,
            db_commands::cancel_browse,
            db_commands::preview_browse_sql,
            db_commands::distinct_values,
            db_commands::preview_replace,
            db_commands::replace_values,
            db_commands::save_table_changes,
            db_commands::run_query,
            db_commands::fetch_rows,
            db_commands::close_results,
            db_commands::cancel_query,
            transfer_commands::export_sql,
            transfer_commands::import_sql,
            transfer_commands::backup_database,
            transfer_commands::read_backup_info,
            transfer_commands::restore_database,
            transfer_commands::cancel_transfer,
            tabular_commands::export_browse,
            tabular_commands::export_result,
            tabular_commands::preview_import,
            tabular_commands::import_rows,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
