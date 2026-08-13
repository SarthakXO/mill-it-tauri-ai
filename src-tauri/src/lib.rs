mod agent;
mod analytics;
mod commands;
mod database;
mod intelligence;
mod llm;
mod schema;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let database = tauri::async_runtime::block_on(database::Database::from_environment());
            let forecast_store = analytics::ForecastStore::new(
                app.path()
                    .app_data_dir()
                    .expect("application data directory should be available"),
            );
            app.manage(database);
            app.manage(forecast_store);
            Ok(())
        })
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            commands::agent::check_ollama,
            commands::agent::list_ollama_models,
            commands::agent::check_database,
            commands::agent::chat_with_ollama,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
