use crate::import::jobs::{Command, ImportQueue, JobChoice};
use crate::library::{editing, AlbumEdit, AlbumRow, Library, TrackEdit, TrackRow};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::State;

pub struct AppState {
    pub lib: Arc<Mutex<Library>>,
    pub queue: ImportQueue,
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

#[tauri::command(async)]
pub fn resolve_job(state: State<AppState>, job_id: u64, choice: JobChoice) -> CmdResult<()> {
    state.queue.send(Command::Resolve { job_id, choice });
    Ok(())
}

#[tauri::command(async)]
pub fn retry_metadata(state: State<AppState>, album_id: i64) -> CmdResult<u64> {
    Ok(state.queue.reimport(album_id))
}

#[tauri::command(async)]
pub fn update_album(state: State<AppState>, album_id: i64, edit: AlbumEdit) -> CmdResult<()> {
    editing::apply_album_edit(&mut state.lib.lock().unwrap(), album_id, &edit).map_err(err)
}

#[tauri::command(async)]
pub fn update_track(state: State<AppState>, track_id: i64, edit: TrackEdit) -> CmdResult<()> {
    editing::apply_track_edit(&mut state.lib.lock().unwrap(), track_id, &edit).map_err(err)
}
