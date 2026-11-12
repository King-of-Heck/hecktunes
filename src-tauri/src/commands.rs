use crate::device::detect::{self, Volume};
use crate::device::plan::PlanSummary;
use crate::device::service::SyncService;
use crate::import::jobs::{Command, ImportQueue, JobChoice};
use crate::library::{
    editing, AlbumEdit, AlbumRow, Library, PlaylistEntry, PlaylistRow, Selection, SelectionKind, SongRow, TrackEdit, TrackRow,
};
use crate::settings::{self, Settings, SettingsView};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, State};

pub struct AppState {
    pub lib: Arc<Mutex<Library>>,
    pub queue: ImportQueue,
    pub sync: SyncService,
    pub player: crate::player::PlayerService,
    pub settings: SettingsState,
}

pub struct SettingsState {
    pub config_dir: PathBuf,
    pub default_root: PathBuf,
    pub active_root: PathBuf,
    /// settings.json as it was when the app started (what is in effect now).
    pub startup: Settings,
    pub key_active: bool,
}

fn settings_view(state: &AppState, saved: &Settings, problem: Option<String>) -> CmdResult<SettingsView> {
    let s = &state.settings;
    let albums = state.lib.lock().unwrap().list_albums().map_err(err)?.len();
    Ok(settings::view(saved, &s.startup, &s.active_root, &s.default_root, s.key_active, problem, albums))
}

#[tauri::command(async)]
pub fn get_settings(state: State<AppState>) -> CmdResult<SettingsView> {
    let (saved, problem) = settings::load_checked(&state.settings.config_dir);
    settings_view(&state, &saved, problem)
}

#[tauri::command(async)]
pub fn set_library_root(state: State<AppState>, path: Option<String>) -> CmdResult<SettingsView> {
    let (saved, _) = settings::load_checked(&state.settings.config_dir);
    let albums = state.lib.lock().unwrap().list_albums().map_err(err)?.len();
    let updated = settings::with_library_root(saved, path.map(PathBuf::from), albums).map_err(err)?;
    settings::save(&state.settings.config_dir, &updated).map_err(err)?;
    settings_view(&state, &updated, None)
}

#[tauri::command(async)]
pub fn set_acoustid_key(state: State<AppState>, key: Option<String>) -> CmdResult<SettingsView> {
    let (saved, _) = settings::load_checked(&state.settings.config_dir);
    let updated = settings::with_acoustid_key(saved, key);
    settings::save(&state.settings.config_dir, &updated).map_err(err)?;
    settings_view(&state, &updated, None)
}

// Every command is `async` so Tauri runs it off the main (UI) thread: several block on the
// Library mutex, which the import worker may hold for a moment while committing an album.
type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[tauri::command(async)]
pub fn list_albums(state: State<AppState>) -> CmdResult<Vec<AlbumRow>> {
    state.lib.lock().unwrap().list_albums().map_err(err)
}

#[tauri::command(async)]
pub fn album_tracks(state: State<AppState>, album_id: i64) -> CmdResult<Vec<TrackRow>> {
    state.lib.lock().unwrap().album_tracks(album_id).map_err(err)
}

#[tauri::command(async)]
pub fn import_paths(state: State<AppState>, paths: Vec<String>) -> CmdResult<()> {
    state.queue.send(Command::Import(paths.into_iter().map(PathBuf::from).collect()));
    Ok(())
}

/// Library files must not be rewritten or deleted while an Echo Mini sync reads them.
fn library_change_allowed(syncing: bool) -> Result<(), String> {
    if syncing {
        Err("Wait for the Echo Mini sync to finish first.".to_string())
    } else {
        Ok(())
    }
}

/// Skip and Search don't touch library files; every other choice does.
fn choice_changes_library(choice: &JobChoice) -> bool {
    !matches!(choice, JobChoice::Skip | JobChoice::Search { .. })
}

#[tauri::command(async)]
pub fn resolve_job(state: State<AppState>, job_id: u64, choice: JobChoice) -> CmdResult<()> {
    if choice_changes_library(&choice) {
        library_change_allowed(state.sync.is_running())?;
    }
    state.queue.send(Command::Resolve { job_id, choice });
    Ok(())
}

#[tauri::command(async)]
pub fn retry_metadata(state: State<AppState>, album_id: i64) -> CmdResult<u64> {
    library_change_allowed(state.sync.is_running())?;
    Ok(state.queue.reimport(album_id))
}

#[tauri::command(async)]
pub fn update_album(state: State<AppState>, album_id: i64, edit: AlbumEdit) -> CmdResult<()> {
    library_change_allowed(state.sync.is_running())?;
    editing::apply_album_edit(&mut state.lib.lock().unwrap(), album_id, &edit).map_err(err)
}

#[tauri::command(async)]
pub fn update_track(state: State<AppState>, track_id: i64, edit: TrackEdit) -> CmdResult<()> {
    library_change_allowed(state.sync.is_running())?;
    editing::apply_track_edit(&mut state.lib.lock().unwrap(), track_id, &edit).map_err(err)
}

#[tauri::command(async)]
pub fn list_songs(state: State<AppState>) -> CmdResult<Vec<SongRow>> {
    state.lib.lock().unwrap().list_songs().map_err(err)
}

/// Removes the album from the library and deletes its folder in the library root.
#[tauri::command(async)]
pub fn delete_album(state: State<AppState>, album_id: i64) -> CmdResult<()> {
    library_change_allowed(state.sync.is_running())?;
    state.lib.lock().unwrap().delete_album(album_id, true).map_err(err)
}

#[tauri::command(async)]
pub fn list_playlists(state: State<AppState>) -> CmdResult<Vec<PlaylistRow>> {
    state.lib.lock().unwrap().list_playlists().map_err(err)
}

#[tauri::command(async)]
pub fn create_playlist(state: State<AppState>, name: String) -> CmdResult<i64> {
    state.lib.lock().unwrap().create_playlist(&name).map_err(err)
}

#[tauri::command(async)]
pub fn rename_playlist(state: State<AppState>, playlist_id: i64, name: String) -> CmdResult<()> {
    state.lib.lock().unwrap().rename_playlist(playlist_id, &name).map_err(err)
}

#[tauri::command(async)]
pub fn delete_playlist(state: State<AppState>, playlist_id: i64) -> CmdResult<()> {
    state.lib.lock().unwrap().delete_playlist(playlist_id).map_err(err)
}

#[tauri::command(async)]
pub fn playlist_entries(state: State<AppState>, playlist_id: i64) -> CmdResult<Vec<PlaylistEntry>> {
    state.lib.lock().unwrap().playlist_entries(playlist_id).map_err(err)
}

#[tauri::command(async)]
pub fn add_to_playlist(state: State<AppState>, playlist_id: i64, track_ids: Vec<i64>) -> CmdResult<()> {
    state.lib.lock().unwrap().add_to_playlist(playlist_id, &track_ids).map_err(err)
}

#[tauri::command(async)]
pub fn remove_playlist_items(state: State<AppState>, playlist_id: i64, item_ids: Vec<i64>) -> CmdResult<()> {
    state.lib.lock().unwrap().remove_playlist_items(playlist_id, &item_ids).map_err(err)
}

#[tauri::command(async)]
pub fn move_playlist_item(state: State<AppState>, playlist_id: i64, item_id: i64, to_index: usize) -> CmdResult<()> {
    state.lib.lock().unwrap().move_playlist_item(playlist_id, item_id, to_index).map_err(err)
}

#[tauri::command(async)]
pub fn get_selection(state: State<AppState>) -> CmdResult<Selection> {
    state.lib.lock().unwrap().selection().map_err(err)
}

#[tauri::command(async)]
pub fn set_selected(state: State<AppState>, kind: SelectionKind, reference: String, on: bool) -> CmdResult<()> {
    state.lib.lock().unwrap().set_selected(kind, &reference, on).map_err(err)
}

#[tauri::command(async)]
pub fn list_volumes() -> Vec<Volume> {
    detect::list_volumes()
}

#[tauri::command(async)]
pub fn preview_sync(state: State<AppState>, card: String) -> CmdResult<PlanSummary> {
    state.sync.preview(&PathBuf::from(card)).map_err(err)
}

/// Starts a sync in the background; progress arrives as "sync" events.
#[tauri::command(async)]
pub fn start_sync(app: AppHandle, state: State<AppState>, card: String) -> CmdResult<()> {
    state
        .sync
        .start(PathBuf::from(card), move |event| {
            let _ = app.emit("sync", event);
        })
        .map_err(err)
}

#[tauri::command(async)]
pub fn cancel_sync(state: State<AppState>) {
    state.sync.cancel();
}

/// Starts previewing a library track; status arrives as "player" events.
#[tauri::command(async)]
pub fn player_play(state: State<AppState>, track_id: i64) -> CmdResult<()> {
    let track = state.lib.lock().unwrap().track(track_id).map_err(err)?.ok_or_else(|| format!("track {track_id} not found"))?;
    state.player.play(track.id, track.file_path, track.duration_ms);
    Ok(())
}

#[tauri::command(async)]
pub fn player_pause(state: State<AppState>) {
    state.player.pause();
}

#[tauri::command(async)]
pub fn player_resume(state: State<AppState>) {
    state.player.resume();
}

#[tauri::command(async)]
pub fn player_seek(state: State<AppState>, position_ms: u64) {
    state.player.seek(position_ms);
}

#[tauri::command(async)]
pub fn player_stop(state: State<AppState>) {
    state.player.stop();
}

#[tauri::command(async)]
pub fn about(app: AppHandle) -> crate::about::AboutInfo {
    use tauri::Manager;
    crate::about::about(app.path().resource_dir().ok().as_deref())
}

#[cfg(test)]
mod guard_tests {
    use super::*;

    #[test]
    fn library_changes_are_refused_only_while_syncing() {
        assert!(library_change_allowed(false).is_ok());
        assert_eq!(library_change_allowed(true).unwrap_err(), "Wait for the Echo Mini sync to finish first.");
    }

    #[test]
    fn only_skip_and_search_leave_the_library_alone() {
        assert!(!choice_changes_library(&JobChoice::Skip));
        assert!(!choice_changes_library(&JobChoice::Search { query: "x".into() }));
        assert!(choice_changes_library(&JobChoice::Pick { mbid: "m".into() }));
        assert!(choice_changes_library(&JobChoice::KeepExisting));
        assert!(choice_changes_library(&JobChoice::Replace));
    }
}
