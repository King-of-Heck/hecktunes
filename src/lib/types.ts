export interface AlbumRow {
  id: number;
  title: string;
  album_artist: string;
  year: string | null;
  genre: string | null;
  mb_release_id: string | null;
  folder_path: string;
  cover_path: string | null;
  needs_metadata: boolean;
  track_count: number;
}

export interface TrackRow {
  id: number;
  album_id: number;
  disc: number;
  number: number;
  title: string;
  artist: string;
  duration_ms: number;
  format: string;
  file_path: string;
  size_bytes: number;
  mb_recording_id: string | null;
  content_hash: string | null;
}

/** A track plus its album's fields (flat, as serialized by Rust). */
export interface SongRow extends TrackRow {
  album_title: string;
  album_artist: string;
  year: string | null;
}

export interface PlaylistRow {
  id: number;
  name: string;
  track_count: number;
  duration_ms: number;
}

/** One line of a playlist; `item_id` identifies the line (a song can appear twice). */
export interface PlaylistEntry extends SongRow {
  item_id: number;
}

export interface CandidateTrack {
  disc: number;
  number: number;
  title: string;
  artist: string;
  length_ms: number | null;
  recording_id: string;
}

export interface ReleaseCandidate {
  mbid: string;
  title: string;
  artist: string;
  date: string | null;
  country: string | null;
  format: string | null;
  genre: string | null;
  disc_count: number;
  tracks: CandidateTrack[];
}

export type JobState =
  | { state: 'queued' }
  | { state: 'identifying' }
  | { state: 'needs_choice'; candidates: ReleaseCandidate[] }
  | { state: 'duplicate'; release: ReleaseCandidate; existing_album_id: number }
  | { state: 'importing' }
  | { state: 'done'; album_id: number; matched: boolean; note?: string }
  | { state: 'skipped' }
  | { state: 'failed'; error: string };

export type JobUpdate = { job_id: number; label: string } & JobState;

export type JobChoice =
  | { kind: 'pick'; mbid: string }
  | { kind: 'keep_existing' }
  | { kind: 'skip' }
  | { kind: 'replace' }
  | { kind: 'search'; query: string };

export interface AlbumEdit {
  title: string;
  album_artist: string;
  year: string | null;
  genre: string | null;
}

export interface TrackEdit {
  title: string;
  artist: string;
  disc: number;
  number: number;
}

export type SelectionKind = 'album' | 'artist' | 'playlist';

export interface Selection {
  albums: number[];
  artists: string[];
  playlists: number[];
}

export interface Volume {
  root: string;
  label: string;
  file_system: string;
  total: number;
  available: number;
  removable: boolean;
  player_internal: boolean;
  hecktunes_card: boolean;
  last_synced: number | null;
}

export interface PlanSummary {
  add: number;
  replace: number;
  remove: number;
  unchanged: number;
  bytes_to_copy: number;
  bytes_freed: number;
  space_needed: number;
  available: number | null;
  fits: boolean;
  warnings: string[];
  first_sync: boolean;
}

export interface SyncProgress {
  phase: 'removing' | 'copying';
  done: number;
  total: number;
  bytes_done: number;
  bytes_total: number;
  current: string;
}

export interface SyncOutcome {
  copied: number;
  removed: number;
  adopted: number;
  conflicts: string[];
  /** Library changes noticed during the sync (a file gone or edited). */
  warnings: string[];
  cancelled: boolean;
}

export type EchoSyncOutcome =
  | { status: 'done'; log: string }
  | { status: 'failed'; code: number | null; log: string }
  | { status: 'missing'; reason: string };

export type SyncEvent =
  | { state: 'preparing' }
  | ({ state: 'progress' } & SyncProgress)
  | { state: 'echo_sync' }
  | { state: 'finished'; outcome: SyncOutcome; echo_sync: EchoSyncOutcome | null; warnings: string[] }
  | { state: 'failed'; error: string };

export type View =
  | { kind: 'albums' }
  | { kind: 'artists'; artist?: string }
  | { kind: 'songs' }
  | { kind: 'needs_metadata' }
  | { kind: 'playlist'; id: number }
  | { kind: 'device' };

export type PlayState = 'idle' | 'playing' | 'paused' | 'ended' | 'error';

export interface PlayerStatus {
  state: PlayState;
  track_id: number | null;
  position_ms: number;
  duration_ms: number;
  error: string | null;
}

/** A song in the list playback was started from (for the bar's title and next/previous). */
export interface QueueItem {
  id: number;
  title: string;
  artist: string;
  album_title: string;
}
