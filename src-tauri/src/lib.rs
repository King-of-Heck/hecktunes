pub mod artwork;
pub mod commands;
pub mod identify;
pub mod import;
pub mod library;
pub mod model;
pub mod settings;
pub mod tags;
#[cfg(test)]
mod test_support;

use commands::AppState;
use identify::{MetadataSource, OnlineSource};
use import::jobs::ImportQueue;
use library::Library;
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager};

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let settings = settings::load(&app.path().app_config_dir()?);
            let root = match settings.library_root.clone() {
                Some(r) => r,
                None => app.path().audio_dir()?.join("HeckTunes"),
            };
            std::fs::create_dir_all(&root)?;
            app.asset_protocol_scope().allow_directory(&root, true)?;
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let lib = Arc::new(Mutex::new(Library::open(&data_dir.join("library.db"), root)?));
            // Settings file, then runtime env var, then the key baked in at build time.
            let key = settings::resolve_acoustid_key(
                settings.acoustid_key,
                std::env::var("HECKTUNES_ACOUSTID_KEY").ok(),
                option_env!("HECKTUNES_ACOUSTID_KEY"),
            );
            let source: Arc<dyn MetadataSource> = Arc::new(OnlineSource::new(key));
            let handle = app.handle().clone();
            let queue = ImportQueue::start(lib.clone(), source, move |update| {
                let _ = handle.emit("import-job", update);
            });
            app.manage(AppState { lib, queue });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_albums,
            commands::album_tracks,
            commands::import_paths,
            commands::resolve_job,
            commands::retry_metadata,
            commands::update_album,
            commands::update_track,
        ])
        .run(tauri::generate_context!())
        .expect("error while running HeckTunes");
}
