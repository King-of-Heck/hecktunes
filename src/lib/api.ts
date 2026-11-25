import { convertFileSrc, invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import type {
  AboutInfo,
  AlbumEdit,
  AlbumRow,
  JobChoice,
  JobUpdate,
  PlanSummary,
  PlayerStatus,
  PlaylistEntry,
  PlaylistRow,
  SettingsView,
  Selection,
  SelectionKind,
  SongRow,
  SyncEvent,
  TrackEdit,
  TrackRow,
  Volume,
} from './types';

export const listAlbums = () => invoke<AlbumRow[]>('list_albums');
export const logProblem = (message: string) => invoke<void>('log_problem', { message });
export const albumTracks = (albumId: number) => invoke<TrackRow[]>('album_tracks', { albumId });
export const importPaths = (paths: string[]) => invoke<void>('import_paths', { paths });
export const importVinyl = (paths: string[]) => invoke<void>('import_vinyl', { paths });
export const vinylPreview = (path: string, atMs: number) => invoke<void>('vinyl_preview', { path, atMs: Math.max(0, Math.round(atMs)) });
export const resolveJob = (jobId: number, choice: JobChoice) => invoke<void>('resolve_job', { jobId, choice });
export const retryMetadata = (albumId: number) => invoke<number>('retry_metadata', { albumId });
export const updateAlbum = (albumId: number, edit: AlbumEdit) => invoke<void>('update_album', { albumId, edit });
export const updateTrack = (trackId: number, edit: TrackEdit) => invoke<void>('update_track', { trackId, edit });
export const setAlbumCover = (albumId: number, path: string) => invoke<void>('set_album_cover', { albumId, path });
export const listSongs = () => invoke<SongRow[]>('list_songs');
export const deleteAlbum = (albumId: number) => invoke<void>('delete_album', { albumId });

export const listPlaylists = () => invoke<PlaylistRow[]>('list_playlists');
export const createPlaylist = (name: string) => invoke<number>('create_playlist', { name });
export const renamePlaylist = (playlistId: number, name: string) => invoke<void>('rename_playlist', { playlistId, name });
export const deletePlaylist = (playlistId: number) => invoke<void>('delete_playlist', { playlistId });
export const playlistEntries = (playlistId: number) => invoke<PlaylistEntry[]>('playlist_entries', { playlistId });
export const addToPlaylist = (playlistId: number, trackIds: number[]) => invoke<void>('add_to_playlist', { playlistId, trackIds });
export const removePlaylistItems = (playlistId: number, itemIds: number[]) =>
  invoke<void>('remove_playlist_items', { playlistId, itemIds });
export const movePlaylistItem = (playlistId: number, itemId: number, toIndex: number) =>
  invoke<void>('move_playlist_item', { playlistId, itemId, toIndex });

export const getSelection = () => invoke<Selection>('get_selection');
export const setSelected = (kind: SelectionKind, reference: string, on: boolean) =>
  invoke<void>('set_selected', { kind, reference, on });
export const listVolumes = () => invoke<Volume[]>('list_volumes');
export const previewSync = (card: string) => invoke<PlanSummary>('preview_sync', { card });
export const startSync = (card: string) => invoke<void>('start_sync', { card });
export const cancelSync = () => invoke<void>('cancel_sync');

/** `bust` changes after edits so the webview reloads a rewritten cover. */
export const coverUrl = (path: string | null, bust = 0) => (path ? `${convertFileSrc(path)}?v=${bust}` : null);

export const onImportJob = (cb: (u: JobUpdate) => void): Promise<UnlistenFn> =>
  listen<JobUpdate>('import-job', (e) => cb(e.payload));
export const onSync = (cb: (e: SyncEvent) => void): Promise<UnlistenFn> => listen<SyncEvent>('sync', (e) => cb(e.payload));

export function onFileDrop(handlers: { hover: (on: boolean) => void; drop: (paths: string[]) => void }): Promise<UnlistenFn> {
  return getCurrentWebview().onDragDropEvent((e) => {
    const p = e.payload;
    if (p.type === 'enter' || p.type === 'over') handlers.hover(true);
    else if (p.type === 'leave') handlers.hover(false);
    else if (p.type === 'drop') {
      handlers.hover(false);
      handlers.drop(p.paths);
    }
  });
}

export function formatDuration(ms: number): string {
  const s = Math.round(ms / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
}

export function formatBytes(n: number): string {
  if (n <= 0) return '0 KB';
  if (n < 1024 ** 2) return `${Math.max(1, Math.round(n / 1024))} KB`;
  if (n < 1024 ** 3) return `${Math.round(n / 1024 ** 2)} MB`;
  return `${(n / 1024 ** 3).toFixed(1)} GB`;
}

export const playTrack = (trackId: number) => invoke<void>('player_play', { trackId });
export const playerPause = () => invoke<void>('player_pause');
export const playerResume = () => invoke<void>('player_resume');
export const playerSeek = (positionMs: number) => invoke<void>('player_seek', { positionMs: Math.max(0, Math.round(positionMs)) });
export const playerStop = () => invoke<void>('player_stop');
/** `save` also writes the volume to settings.json; pass false while a slider is still moving. */
export const playerSetVolume = (volume: number, muted: boolean, save: boolean) =>
  invoke<void>('player_set_volume', { volume: Math.min(100, Math.max(0, Math.round(volume))), muted, save });
export const onPlayer = (cb: (s: PlayerStatus) => void): Promise<UnlistenFn> => listen<PlayerStatus>('player', (e) => cb(e.payload));

export const getSettings = () => invoke<SettingsView>('get_settings');
export const setLibraryRoot = (path: string | null) => invoke<SettingsView>('set_library_root', { path });
export const setAcoustidKey = (key: string | null) => invoke<SettingsView>('set_acoustid_key', { key });

export const about = () => invoke<AboutInfo>('about');
