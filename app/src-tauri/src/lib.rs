mod commands;
mod db;
mod write_queue;

use commands::AppState;
use tauri::Manager;
use write_queue::WriteQueue;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;

            let db_path = data_dir.join("qurator.sqlite");
            let conn = db::open(&db_path)?;
            let default_collection_id = db::ensure_default_collection(&conn)?;
            let write_queue = WriteQueue::start(conn);

            app.manage(AppState {
                write_queue,
                db_path,
                default_collection_id,
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::import_paths,
            commands::list_works
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
