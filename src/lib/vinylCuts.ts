import type { PlanTrack, ReleaseCandidate } from './types';

export const NUDGE_MS = 500;
export const BIG_NUDGE_MS = 5000;
/** No track may be shorter than this. */
export const MIN_TRACK_MS = 1000;

/** "3:07.5" */
export function formatCutTime(ms: number): string {
  const tenths = Math.round(ms / 100);
  const s = Math.floor(tenths / 10);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}.${tenths % 10}`;
}

const clamp = (v: number, lo: number, hi: number) => Math.min(Math.max(v, lo), hi);

/** Moves track i's start; the previous track on the same file ends where it starts. */
export function nudgeStart(tracks: PlanTrack[], i: number, deltaMs: number): PlanTrack[] {
  const out = tracks.map((t) => ({ ...t }));
  const t = out[i];
  const prev = i > 0 && out[i - 1].file === t.file ? out[i - 1] : null;
  const lo = prev ? prev.start_ms + MIN_TRACK_MS : 0;
  const start = clamp(t.start_ms + deltaMs, lo, t.end_ms - MIN_TRACK_MS);
  if (prev && prev.end_ms === t.start_ms) prev.end_ms = start;
  t.start_ms = start;
  if (i > 0 && prev) t.cut = 'manual';
  return out;
}

/** Moves the end of track i (only meaningful for the last track on its file). */
export function nudgeEnd(tracks: PlanTrack[], i: number, deltaMs: number, fileDurationMs: number): PlanTrack[] {
  const out = tracks.map((t) => ({ ...t }));
  out[i].end_ms = clamp(out[i].end_ms + deltaMs, out[i].start_ms + MIN_TRACK_MS, fileDurationMs);
  return out;
}

/** Splits track i at `atMs`; returns the list unchanged if the point is too close to either end. */
export function splitAt(tracks: PlanTrack[], i: number, atMs: number): PlanTrack[] {
  const t = tracks[i];
  if (atMs < t.start_ms + MIN_TRACK_MS || atMs > t.end_ms - MIN_TRACK_MS) return tracks;
  const first = { ...t, end_ms: atMs };
  const second = { ...t, start_ms: atMs, cut: 'manual' as const };
  return [...tracks.slice(0, i), first, second, ...tracks.slice(i + 1)];
}

/** Merges track i into the one before it (same file only). */
export function removeCut(tracks: PlanTrack[], i: number): PlanTrack[] {
  if (i === 0 || tracks[i - 1].file !== tracks[i].file) return tracks;
  const merged = { ...tracks[i - 1], end_ms: tracks[i].end_ms };
  return [...tracks.slice(0, i - 1), merged, ...tracks.slice(i + 1)];
}

/** Titles, discs and numbers follow the release's track order; extra pieces are "Track n". */
export function retitle(tracks: PlanTrack[], release: ReleaseCandidate | null): PlanTrack[] {
  return tracks.map((t, i) => {
    const rt = release?.tracks[i];
    return rt ? { ...t, title: rt.title, disc: rt.disc, number: rt.number } : { ...t, title: `Track ${i + 1}`, disc: 1, number: i + 1 };
  });
}
