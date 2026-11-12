pub mod artwork;
pub mod commands;
pub mod device;
pub mod identify;
pub mod import;
pub mod library;
pub mod model;
pub mod player;
pub mod settings;
pub mod tags;
#[cfg(test)]
mod test_support;

use commands::AppState;
use device::service::SyncService;
use identify::{MetadataSource, OnlineSource};
use import::jobs::ImportQueue;
use library::Library;
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager};

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let config_dir = app.path().app_config_dir()?;
            let (settings, settings_problem) = settings::load_checked(&config_dir);
            if let Some(p) = &settings_problem {
                eprintln!("{}", p.chars().map(|c| if c.is_ascii() { c } else { '?' }).collect::<String>());
            }
            let default_root = app.path().audio_dir()?.join("HeckTunes");
            let root = settings.library_root.clone().unwrap_or_else(|| default_root.clone());
            let startup = settings.clone();
            std::fs::create_dir_all(&root)?;
            app.asset_protocol_scope().allow_directory(&root, true)?;
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let active_root = root.clone();
            let lib = Arc::new(Mutex::new(Library::open(&data_dir.join("library.db"), root)?));
            // Settings file, then runtime env var, then the key baked in at build time.
            let key = settings::resolve_acoustid_key(
                settings.acoustid_key,
                std::env::var("HECKTUNES_ACOUSTID_KEY").ok(),
                option_env!("HECKTUNES_ACOUSTID_KEY"),
            );
            let key_active = key.is_some();
            let source: Arc<dyn MetadataSource> = Arc::new(OnlineSource::new(key));
            let handle = app.handle().clone();
            let queue = ImportQueue::start(lib.clone(), source, move |update| {
                let _ = handle.emit("import-job", update);
            });
            let sync = SyncService::new(lib.clone(), device::echosync::sidecar_path());
            let player_handle = app.handle().clone();
            let player = player::PlayerService::start(
                || Ok(Box::new(player::RodioAudio::open()?) as Box<dyn player::Audio>),
                move |status| {
                    let _ = player_handle.emit("player", status);
                },
            );
            let settings = commands::SettingsState { config_dir, default_root, active_root, startup, key_active };
            app.manage(AppState { lib, queue, sync, player, settings });
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
            commands::list_songs,
            commands::delete_album,
            commands::list_playlists,
            commands::create_playlist,
            commands::rename_playlist,
            commands::delete_playlist,
            commands::playlist_entries,
            commands::add_to_playlist,
            commands::remove_playlist_items,
            commands::move_playlist_item,
            commands::get_selection,
            commands::set_selected,
            commands::list_volumes,
            commands::preview_sync,
            commands::start_sync,
            commands::cancel_sync,
            commands::player_play,
            commands::player_pause,
            commands::player_resume,
            commands::player_seek,
            commands::player_stop,
            commands::get_settings,
            commands::set_library_root,
            commands::set_acoustid_key,
        ])
        .run(tauri::generate_context!())
        .expect("error while running HeckTunes");
}
