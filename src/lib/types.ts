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
  | { state: 'done'; album_id: number; matched: boolean }
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

export type View = 'albums' | 'needs_metadata';
