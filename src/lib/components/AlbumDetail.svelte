<script lang="ts">
  import { albumTracks, coverUrl, formatDuration, retryMetadata } from '../api';
  import type { AlbumRow, JobUpdate, TrackRow } from '../types';
  let { album, bust, jobs, onBack, onEdit }: { album: AlbumRow; bust: number; jobs: Record<number, JobUpdate>; onBack: () => void; onEdit: (tracks: TrackRow[]) => void } = $props();

  let tracks = $state<TrackRow[]>([]);
  // retry_metadata returns no job id, so identify our re-identify job from the jobs map:
  // the first job newer than the click whose label matches this album (see Worker::reimport).
  let since = $state<number | null>(null);
  let myJobId = $state<number | null>(null);
  let retryError = $state<string | null>(null);

  const labels = $derived([`${album.album_artist} – ${album.title}`, `album ${album.id}`]);
  $effect(() => {
    if (since === null || myJobId !== null) return;
    const found = Object.values(jobs).find((j) => j.job_id > since! && labels.includes(j.label));
    if (found) myJobId = found.job_id;
  });
  const terminal = (st: string) => st === 'done' || st === 'failed' || st === 'skipped';
  // Busy from the click until our job exists and reaches a terminal state (or is cleared).
  const retrying = $derived(
    since !== null && (myJobId === null || (jobs[myJobId] !== undefined && !terminal(jobs[myJobId].state))),
  );

  $effect(() => {
    // re-load whenever the album row (or bust counter) changes
    void bust;
    let cancelled = false;
    albumTracks(album.id)
      .then((t) => { if (!cancelled) tracks = t; })
      .catch((e) => console.error('album_tracks failed', e));
    return () => { cancelled = true; };
  });

  async function fix() {
    retryError = null;
    myJobId = null;
    since = Math.max(0, ...Object.keys(jobs).map(Number));
    try {
      await retryMetadata(album.id);
    } catch (e) {
      console.error('retry_metadata failed', e);
      retryError = `Couldn't start lookup: ${e}`;
      since = null;
    }
  }
</script>

<div class="detail">
  <button class="back" onclick={onBack}>← Albums</button>
  <header>
    {#if album.cover_path}<img src={coverUrl(album.cover_path, bust)} alt="" />{:else}<div class="noart">♪</div>{/if}
    <div>
      <h2>{album.title}</h2>
      <p class="artist">{album.album_artist}</p>
      <p class="meta">{[album.year, album.genre, `${album.track_count} tracks`].filter(Boolean).join(' · ')}</p>
      <div class="actions">
        <button onclick={() => onEdit(tracks)}>Edit tags</button>
        <button onclick={fix} disabled={retrying}>{retrying ? 'Looking up…' : 'Fix metadata'}</button>
      </div>
      {#if retryError}<p class="err">{retryError}</p>{/if}
    </div>
  </header>
  <table>
    <tbody>
      {#each tracks as t (t.id)}
        <tr>
          <td class="num">{tracks.some((x) => x.disc > 1) ? `${t.disc}-` : ''}{t.number}</td>
          <td>{t.title}{#if t.artist !== album.album_artist}<span class="feat"> — {t.artist}</span>{/if}</td>
          <td class="fmt">{t.format.toUpperCase()}</td>
          <td class="dur">{formatDuration(t.duration_ms)}</td>
        </tr>
      {/each}
    </tbody>
  </table>
</div>

<style>
  .detail { padding: 16px 24px; overflow: auto; height: 100%; }
  .back { border: none; background: none; color: var(--accent); padding: 0; margin-bottom: 12px; }
  header { display: flex; gap: 20px; align-items: flex-end; margin-bottom: 20px; }
  img, .noart { width: 180px; height: 180px; border-radius: var(--radius); object-fit: cover; background: var(--panel); }
  .noart { display: grid; place-items: center; font-size: 48px; color: var(--muted); }
  h2 { margin: 0 0 4px; font-size: 24px; }
  .artist { margin: 0; font-size: 16px; color: var(--accent); }
  .meta { margin: 4px 0 12px; color: var(--muted); }
  .err { margin: 8px 0 0; color: #e5484d; font-size: 13px; }
  .actions { display: flex; gap: 8px; }
  table { width: 100%; border-collapse: collapse; }
  td { padding: 7px 8px; border-bottom: 1px solid var(--border); }
  .num, .dur, .fmt { color: var(--muted); width: 1%; white-space: nowrap; }
  .feat { color: var(--muted); }
</style>
