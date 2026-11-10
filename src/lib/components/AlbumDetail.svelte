<script lang="ts">
  import { albumTracks, coverUrl, formatDuration, retryMetadata } from '../api';
  import type { AlbumRow, TrackRow } from '../types';
  let { album, bust, onBack, onEdit }: { album: AlbumRow; bust: number; onBack: () => void; onEdit: (tracks: TrackRow[]) => void } = $props();

  let tracks = $state<TrackRow[]>([]);
  let retrying = $state(false);

  $effect(() => {
    // re-load whenever the album row (or bust counter) changes
    void bust;
    albumTracks(album.id).then((t) => (tracks = t));
  });

  async function fix() {
    retrying = true;
    await retryMetadata(album.id);
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
  .actions { display: flex; gap: 8px; }
  table { width: 100%; border-collapse: collapse; }
  td { padding: 7px 8px; border-bottom: 1px solid var(--border); }
  .num, .dur, .fmt { color: var(--muted); width: 1%; white-space: nowrap; }
  .feat { color: var(--muted); }
</style>
