use crate::convert::ConvertService;
use crate::device::detect::{self, Volume};
use crate::device::plan::PlanSummary;
use crate::device::service::SyncService;
use crate::import::jobs::{Command, ImportQueue, JobChoice};
use crate::library::{
    editing, AlbumEdit, AlbumRow, Library, PlaylistEntry, PlaylistRow, Selection, SelectionKind, SongRow, SyncAs, TrackEdit, TrackRow,
};
use crate::settings::{self, Settings, SettingsView};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, State};

pub struct AppState {
    pub lib: Arc<Mutex<Library>>,
    pub queue: ImportQueue,
    pub sync: SyncService,
    /// Convert to AAC.
    pub convert: ConvertService,
    pub player: crate::player::PlayerService,
    pub settings: SettingsState,
    /// False when the library folder was missing at startup (e.g. its drive isn't connected):
    /// commands that change the library or read its files for a sync refuse until a restart.
    pub library_available: bool,
    /// Temporary FLACs of vinyl imports and the Listen clip.
    pub vinyl_tmp: PathBuf,
    /// Held for a whole `vinyl_preview`, so two quick Listen clicks don't both rewrite the clip.
    pub preview_lock: Mutex<()>,
}

pub struct SettingsState {
    pub config_dir: PathBuf,
    pub default_root: PathBuf,
    pub active_root: PathBuf,
    /// settings.json as it was when the app started.
    pub startup: Settings,
    /// The settings in effect now: settings.json's, with the library folder actually chosen at
    /// startup (which may come from the library database when settings.json is damaged).
    pub in_effect: Settings,
    pub key_active: bool,
    /// Held across each read-change-write of settings.json, so quick volume saves can't
    /// overwrite another setting saved at the same moment.
    pub save_lock: Mutex<()>,
}

fn settings_view(state: &AppState, saved: &Settings, problem: Option<String>) -> CmdResult<SettingsView> {
    let s = &state.settings;
    let albums = state.lib.lock().unwrap().album_count().map_err(err)?;
    Ok(settings::view(saved, &s.startup, &s.active_root, &s.default_root, s.key_active, problem, albums, state.library_available))
}

/// settings.json to build a save on; a damaged file is replaced by the settings in effect now.
fn settings_to_update(state: &AppState) -> Settings {
    let (saved, problem) = settings::load_checked(&state.settings.config_dir);
    settings::save_base(saved, problem.is_some(), &state.settings.in_effect)
}

#[tauri::command(async)]
pub fn get_settings(state: State<AppState>) -> CmdResult<SettingsView> {
    let (saved, problem) = settings::load_checked(&state.settings.config_dir);
    settings_view(&state, &saved, problem)
}

#[tauri::command(async)]
pub fn set_library_root(state: State<AppState>, path: Option<String>) -> CmdResult<SettingsView> {
    let _saving = state.settings.save_lock.lock().unwrap_or_else(|p| p.into_inner());
    let base = settings_to_update(&state);
    let albums = state.lib.lock().unwrap().album_count().map_err(err)?;
    let updated = settings::with_library_root(base, path.map(PathBuf::from), albums).map_err(err)?;
    settings::save(&state.settings.config_dir, &updated).map_err(err)?;
    // The library is empty, so recording the new folder now loses nothing; it keeps a later
    // fallback to the recorded folder from undoing this choice.
    let next_root = updated.library_root.clone().unwrap_or_else(|| state.settings.default_root.clone());
    state.lib.lock().unwrap().record_root(&next_root).map_err(err)?;
    settings_view(&state, &updated, None)
}

#[tauri::command(async)]
pub fn set_acoustid_key(state: State<AppState>, key: Option<String>) -> CmdResult<SettingsView> {
    let _saving = state.settings.save_lock.lock().unwrap_or_else(|p| p.into_inner());
    let updated = settings::with_acoustid_key(settings_to_update(&state), key);
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
    let albums = state.lib.lock().unwrap().list_albums().map_err(err);
    match &albums {
        Ok(a) => crate::diag::log(&format!("list_albums: {} albums", a.len())),
        Err(e) => crate::diag::log(&format!("list_albums failed: {e}")),
    }
    albums
}

/// Records a problem the window noticed (a slow or failed load, a script error) in the diagnostic log.
#[tauri::command]
pub fn log_problem(message: String) {
    crate::diag::log(&format!("window: {}", message.chars().take(2000).collect::<String>()));
}

#[tauri::command(async)]
pub fn album_tracks(state: State<AppState>, album_id: i64) -> CmdResult<Vec<TrackRow>> {
    state.lib.lock().unwrap().album_tracks(album_id).map_err(err)
}

#[tauri::command(async)]
pub fn import_paths(state: State<AppState>, paths: Vec<String>) -> CmdResult<()> {
    library_ready(&state)?;
    state.queue.send(Command::Import(paths.into_iter().map(PathBuf::from).collect()));
    Ok(())
}

#[tauri::command(async)]
pub fn import_vinyl(state: State<AppState>, paths: Vec<String>) -> CmdResult<()> {
    library_ready(&state)?;
    state.queue.send(Command::ImportVinyl(paths.into_iter().map(PathBuf::from).collect()));
    Ok(())
}

/// Half the length of the Listen clip around a cut.
const PREVIEW_HALF_MS: u64 = 3000;

/// The clip `[from, to)` in ms played to check a cut at `at_ms`: up to 3 s either side, inside the
/// recording, never empty. None when the recording has no length.
fn preview_window(at_ms: u64, duration_ms: u64) -> Option<(u64, u64)> {
    if duration_ms == 0 {
        return None;
    }
    let at = at_ms.min(duration_ms);
    Some((at.saturating_sub(PREVIEW_HALF_MS), (at + PREVIEW_HALF_MS).min(duration_ms)))
}

/// Plays the 6 s around `at_ms` of a vinyl recording, so a cut can be checked by ear.
#[tauri::command(async)]
pub fn vinyl_preview(state: State<AppState>, path: String, at_ms: u64) -> CmdResult<()> {
    // One Listen at a time: a second click must not rewrite the clip while the first writes or plays it.
    let _one_at_a_time = state.preview_lock.lock().unwrap_or_else(|p| p.into_inner());
    let info = crate::vinyl::source::probe(std::path::Path::new(&path)).map_err(|e| format!("{e:#}"))?;
    let (from, to) = preview_window(at_ms, info.duration_ms()).ok_or("That recording has no audio to play.")?;
    // The player may be holding the previous clip open; it must let go before it is rewritten.
    if !state.player.stop_and_wait(PLAYER_STOP_TIMEOUT) {
        return Err("The player is busy. Try again in a moment.".into());
    }
    std::fs::create_dir_all(&state.vinyl_tmp).map_err(err)?;
    let clip = state.vinyl_tmp.join("preview.flac");
    crate::vinyl::encode::encode_range(&info, info.ms_to_frame(from), info.ms_to_frame(to), &clip).map_err(|e| format!("{e:#}"))?;
    state.player.play(crate::player::PREVIEW_TRACK_ID, clip, to.saturating_sub(from));
    Ok(())
}

/// Library files must not be rewritten or deleted while an HeckPod sync reads them, or while
/// albums are being converted to AAC (ffmpeg reads the FLACs and writes beside them).
fn library_change_allowed(syncing: bool, converting: bool) -> Result<(), String> {
    if syncing {
        Err("Wait for the HeckPod sync to finish first.".to_string())
    } else if converting {
        Err("Wait for the AAC conversion to finish first.".to_string())
    } else {
        Ok(())
    }
}

/// Nothing may write to or sync from a library folder that isn't there.
fn library_usable(available: bool, root: &std::path::Path) -> Result<(), String> {
    if available {
        Ok(())
    } else {
        Err(settings::unavailable_message(root))
    }
}

fn library_ready(state: &AppState) -> Result<(), String> {
    library_usable(state.library_available, &state.settings.active_root)
}

/// Library files may change: the folder is there and no sync is reading it.
fn library_change_ready(state: &AppState) -> Result<(), String> {
    library_ready(state)?;
    library_change_allowed(state.sync.is_running(), state.convert.is_busy())
}

/// How long to wait for the player to let go of a file before refusing a library change.
const PLAYER_STOP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Stops playback first when the player holds a file of `album_id`, so the album's files can be
/// rewritten or removed. The Library lock is held only to look the track up, never while waiting.
fn release_album_files(player: &crate::player::PlayerService, lib: &Mutex<Library>, album_id: i64) -> Result<(), String> {
    let Some(track_id) = player.current_track() else { return Ok(()) };
    let playing_album = lib.lock().unwrap().track(track_id).map_err(err)?.map(|t| t.album_id);
    if playing_album != Some(album_id) {
        return Ok(());
    }
    if player.stop_and_wait(PLAYER_STOP_TIMEOUT) {
        Ok(())
    } else {
        Err("The player is still holding a file from this album. Try again in a moment.".into())
    }
}

/// Skip, Search and choosing another release for a vinyl review don't touch library files.
fn choice_changes_library(choice: &JobChoice) -> bool {
    !matches!(choice, JobChoice::Skip | JobChoice::Search { .. } | JobChoice::VinylRealign { .. })
}

#[tauri::command(async)]
pub fn resolve_job(state: State<AppState>, job_id: u64, choice: JobChoice) -> CmdResult<()> {
    if choice_changes_library(&choice) {
        library_change_ready(&state)?;
    }
    state.queue.send(Command::Resolve { job_id, choice });
    Ok(())
}

#[tauri::command(async)]
pub fn retry_metadata(state: State<AppState>, album_id: i64) -> CmdResult<u64> {
    library_change_ready(&state)?;
    release_album_files(&state.player, &state.lib, album_id)?;
    Ok(state.queue.reimport(album_id))
}

#[tauri::command(async)]
pub fn update_album(state: State<AppState>, album_id: i64, edit: AlbumEdit) -> CmdResult<()> {
    library_change_ready(&state)?;
    release_album_files(&state.player, &state.lib, album_id)?;
    editing::apply_album_edit(&mut state.lib.lock().unwrap(), album_id, &edit).map_err(err)
}

/// Largest image file accepted as a new cover.
const MAX_COVER_FILE_BYTES: u64 = 25 * 1024 * 1024;

#[tauri::command(async)]
pub fn set_album_cover(state: State<AppState>, album_id: i64, path: String) -> CmdResult<()> {
    library_change_ready(&state)?;
    let size = std::fs::metadata(&path).map_err(|e| format!("Couldn't read that file: {e}"))?.len();
    if size > MAX_COVER_FILE_BYTES {
        return Err("That image is larger than 25 MB.".to_string());
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("Couldn't read that file: {e}"))?;
    release_album_files(&state.player, &state.lib, album_id)?;
    editing::set_album_cover(&mut state.lib.lock().unwrap(), album_id, &bytes).map_err(err)
}

#[tauri::command(async)]
pub fn update_track(state: State<AppState>, track_id: i64, edit: TrackEdit) -> CmdResult<()> {
    library_change_ready(&state)?;
    let album_id = state.lib.lock().unwrap().track(track_id).map_err(err)?.map(|t| t.album_id);
    if let Some(album_id) = album_id {
        release_album_files(&state.player, &state.lib, album_id)?;
    }
    editing::apply_track_edit(&mut state.lib.lock().unwrap(), track_id, &edit).map_err(err)
}

#[tauri::command(async)]
pub fn list_songs(state: State<AppState>) -> CmdResult<Vec<SongRow>> {
    state.lib.lock().unwrap().list_songs().map_err(err)
}

/// Why Convert to AAC can't be used on this install (`None` when it can).
#[tauri::command(async)]
pub fn convert_unavailable(state: State<AppState>) -> Option<String> {
    state.convert.unavailable_reason()
}

/// Queues the album for Convert to AAC; progress arrives as "convert" events.
#[tauri::command(async)]
pub fn convert_album(state: State<AppState>, album_id: i64, options: crate::convert::ConvertOptions) -> CmdResult<()> {
    library_ready(&state)?;
    if state.sync.is_running() {
        return Err("Wait for the HeckPod sync to finish first.".to_string());
    }
    state.convert.enqueue(album_id, options).map_err(err)
}

#[tauri::command(async)]
pub fn cancel_convert(state: State<AppState>, album_id: i64) -> CmdResult<()> {
    state.convert.cancel(album_id);
    Ok(())
}

/// Deletes the album's AAC copies and sets it back to syncing its FLAC files.
#[tauri::command(async)]
pub fn remove_aac(state: State<AppState>, album_id: i64) -> CmdResult<()> {
    library_change_ready(&state)?;
    crate::convert::remove_copies(&mut state.lib.lock().unwrap(), album_id).map(|_| ()).map_err(err)
}

/// Chooses whether the album's FLAC files or its AAC copies go to the HeckPod.
#[tauri::command(async)]
pub fn set_album_sync_as(state: State<AppState>, album_id: i64, sync_as: SyncAs) -> CmdResult<()> {
    library_ready(&state)?;
    state.lib.lock().unwrap().set_album_sync_as(album_id, sync_as).map_err(err)
}

/// Removes the album from the library and deletes its folder in the library root.
#[tauri::command(async)]
pub fn delete_album(state: State<AppState>, album_id: i64) -> CmdResult<()> {
    library_change_ready(&state)?;
    release_album_files(&state.player, &state.lib, album_id)?;
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
    library_ready(&state)?;
    state.sync.preview(&PathBuf::from(card)).map_err(err)
}

/// Starts a sync in the background; progress arrives as "sync" events.
#[tauri::command(async)]
pub fn start_sync(app: AppHandle, state: State<AppState>, card: String) -> CmdResult<()> {
    library_ready(&state)?;
    if state.convert.is_busy() {
        return Err("Wait for the AAC conversion to finish first.".to_string());
    }
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

/// Sets the player volume (0-100) and mute at once; `save` also writes them to settings.json
/// (the UI saves when a slider drag ends, not on every step of it).
#[tauri::command(async)]
pub fn player_set_volume(state: State<AppState>, volume: u8, muted: bool, save: bool) -> CmdResult<()> {
    state.player.set_gain(crate::player::gain(volume, muted));
    if save {
        let _saving = state.settings.save_lock.lock().unwrap_or_else(|p| p.into_inner());
        let updated = settings::with_volume(settings_to_update(&state), volume, muted);
        settings::save(&state.settings.config_dir, &updated).map_err(err)?;
    }
    Ok(())
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
        assert!(library_change_allowed(false, false).is_ok());
        assert_eq!(library_change_allowed(true, false).unwrap_err(), "Wait for the HeckPod sync to finish first.");
        assert_eq!(library_change_allowed(true, true).unwrap_err(), "Wait for the HeckPod sync to finish first.");
        assert_eq!(library_change_allowed(false, true).unwrap_err(), "Wait for the AAC conversion to finish first.");
    }

    #[test]
    fn library_changes_are_refused_while_the_folder_is_unavailable() {
        assert!(library_usable(true, std::path::Path::new("E:/Tunes")).is_ok());
        assert_eq!(
            library_usable(false, std::path::Path::new("E:/Tunes")).unwrap_err(),
            "Your library folder E:/Tunes isn't available (is the drive connected?) Connect it and restart HeckTunes."
        );
    }

    #[test]
    fn the_listen_window_stays_inside_the_recording() {
        assert_eq!(preview_window(10_000, 60_000), Some((7_000, 13_000)));
        assert_eq!(preview_window(1_000, 60_000), Some((0, 4_000)));
        assert_eq!(preview_window(59_000, 60_000), Some((56_000, 60_000)));
        // A cut at or past the end (e.g. the last track's end) still plays the last 3 s.
        assert_eq!(preview_window(60_000, 60_000), Some((57_000, 60_000)));
        assert_eq!(preview_window(99_000, 60_000), Some((57_000, 60_000)));
        assert_eq!(preview_window(0, 0), None);
    }

    #[test]
    fn only_skip_and_search_leave_the_library_alone() {
        assert!(!choice_changes_library(&JobChoice::Skip));
        assert!(!choice_changes_library(&JobChoice::Search { query: "x".into() }));
        assert!(choice_changes_library(&JobChoice::Pick { mbid: "m".into() }));
        assert!(choice_changes_library(&JobChoice::KeepExisting));
        assert!(choice_changes_library(&JobChoice::Replace));
        assert!(!choice_changes_library(&JobChoice::VinylRealign { mbid: "m".into() }));
        assert!(choice_changes_library(&JobChoice::VinylConfirm { tracks: vec![], matched: false }));
    }
}

#[cfg(test)]
mod player_release_tests {
    use super::*;
    use crate::player::{Audio, PlayerService};
    use crate::test_support::add_memory_album;
    use std::path::Path;
    use std::time::{Duration, Instant};

    struct SilentAudio(Arc<Mutex<bool>>);

    impl Audio for SilentAudio {
        fn load(&mut self, _path: &Path) -> anyhow::Result<Option<u64>> {
            *self.0.lock().unwrap() = true;
            Ok(Some(1_000))
        }
        fn pause(&mut self) {}
        fn resume(&mut self) {}
        fn seek(&mut self, _ms: u64) -> anyhow::Result<()> {
            Ok(())
        }
        fn stop(&mut self) {
            *self.0.lock().unwrap() = false;
        }
        fn position_ms(&self) -> u64 {
            0
        }
        fn finished(&self) -> bool {
            false
        }
        fn set_gain(&mut self, _gain: f32) {}
    }

    fn playing(track_id: i64) -> (PlayerService, Arc<Mutex<bool>>) {
        let open = Arc::new(Mutex::new(false));
        let o = open.clone();
        let player = PlayerService::start(move || Ok(Box::new(SilentAudio(o.clone())) as Box<dyn Audio>), |_| {});
        player.play(track_id, PathBuf::from("x.flac"), 1_000);
        let deadline = Instant::now() + Duration::from_secs(5);
        while player.current_track() != Some(track_id) {
            assert!(Instant::now() < deadline, "player never started");
            std::thread::sleep(Duration::from_millis(5));
        }
        (player, open)
    }

    #[test]
    fn playback_of_the_affected_album_is_stopped_first() {
        let mut lib = Library::open_in_memory(PathBuf::from("/lib")).unwrap();
        let (a, a_tracks) = add_memory_album(&mut lib, "A", "X", 2);
        let (b, _) = add_memory_album(&mut lib, "B", "X", 1);
        let lib = Mutex::new(lib);
        let (player, open) = playing(a_tracks[1]);

        release_album_files(&player, &lib, b).unwrap();
        assert_eq!(player.current_track(), Some(a_tracks[1]), "another album's playback is left alone");
        assert!(*open.lock().unwrap());

        release_album_files(&player, &lib, a).unwrap();
        assert_eq!(player.current_track(), None);
        assert!(!*open.lock().unwrap(), "the file was let go before returning");
    }

    #[test]
    fn nothing_playing_needs_nothing() {
        let lib = Mutex::new(Library::open_in_memory(PathBuf::from("/lib")).unwrap());
        let player = PlayerService::start(|| anyhow::bail!("no device"), |_| {});
        release_album_files(&player, &lib, 1).unwrap();
    }
}
