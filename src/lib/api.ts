import { convertFileSrc, invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import type { AlbumEdit, AlbumRow, JobChoice, JobUpdate, TrackEdit, TrackRow } from './types';

export const listAlbums = () => invoke<AlbumRow[]>('list_albums');
export const albumTracks = (albumId: number) => invoke<TrackRow[]>('album_tracks', { albumId });
export const importPaths = (paths: string[]) => invoke<void>('import_paths', { paths });
export const resolveJob = (jobId: number, choice: JobChoice) => invoke<void>('resolve_job', { jobId, choice });
export const retryMetadata = (albumId: number) => invoke<number>('retry_metadata', { albumId });
export const updateAlbum = (albumId: number, edit: AlbumEdit) => invoke<void>('update_album', { albumId, edit });
export const updateTrack = (trackId: number, edit: TrackEdit) => invoke<void>('update_track', { trackId, edit });

/** `bust` changes after edits so the webview reloads a rewritten cover. */
export const coverUrl = (path: string | null, bust = 0) => (path ? `${convertFileSrc(path)}?v=${bust}` : null);

export const onImportJob = (cb: (u: JobUpdate) => void): Promise<UnlistenFn> =>
  listen<JobUpdate>('import-job', (e) => cb(e.payload));

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
