<script lang="ts">
  import { inControl } from '../rows';
  import { albumTracks, coverUrl, deleteAlbum, formatDuration } from '../api';
  import type { AlbumRow, JobUpdate, PlaylistRow, QueueItem, TrackRow } from '../types';
  import AddToPlaylist from './AddToPlaylist.svelte';
  let {
    album,
    bust,
    jobs,
    playlists,
    retryJobId,
    syncing,
    nowPlayingId,
    onPlay,
    onRetry,
    onBack,
    onEdit,
    onPlaylistsChanged,
    onRemoved,
  }: {
    album: AlbumRow;
    bust: number;
    jobs: Record<number, JobUpdate>;
    playlists: PlaylistRow[];
    retryJobId: number | null;
    /** A sync is copying library files: hold back changes to them until it ends. */
    syncing: boolean;
    nowPlayingId: number | null;
    onPlay: (items: QueueItem[], index: number) => void;
    onRetry: () => Promise<void>;
    onBack: () => void;
    onEdit: (tracks: TrackRow[]) => void;
    onPlaylistsChanged: () => Promise<void>;
    onRemoved: () => Promise<void>;
  } = $props();

  let tracks = $state<TrackRow[]>([]);
  const items = $derived(tracks.map((t) => ({ id: t.id, title: t.title, artist: t.artist, album_title: album.title })));
  let retryError = $state<string | null>(null);
  let confirming = $state(false);
  let removing = $state(false);
  let removeError = $state<string | null>(null);

  async function remove() {
    removing = true;
    removeError = null;
    try {
      await deleteAlbum(album.id);
    } catch (e) {
      removeError = `Couldn't remove: ${e}`;
      removing = false;
      return;
    }
    await onRemoved().catch((e) => console.error('refresh after remove failed', e));
  }

  const terminal = (st: string) => st === 'done' || st === 'failed' || st === 'skipped';
  // Busy while our retry job exists and has not finished.
  const retrying = $derived(retryJobId !== null && jobs[retryJobId] !== undefined && !terminal(jobs[retryJobId].state));

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
    try {
      await onRetry();
    } catch (e) {
      console.error('retry_metadata failed', e);
      retryError = `Couldn't start lookup: ${e}`;
    }
  }
</script>

<div class="detail">
  <button class="back" onclick={onBack}>← Back</button>
  <header>
    {#if album.cover_path}<img src={coverUrl(album.cover_path, bust)} alt="" />{:else}<div class="noart">♪</div>{/if}
    <div>
      <h2>{album.title}</h2>
      <p class="artist">{album.album_artist}</p>
      <p class="meta">{[album.year, album.genre, `${album.track_count} tracks`].filter(Boolean).join(' · ')}</p>
      <div class="actions">
        <button onclick={() => onEdit(tracks)} disabled={syncing}>Edit tags</button>
        <button onclick={fix} disabled={retrying || syncing}>{retrying ? 'Looking up…' : 'Fix metadata'}</button>
        <AddToPlaylist trackIds={tracks.map((t) => t.id)} {playlists} {onPlaylistsChanged} label="Add to playlist" />
        {#if !confirming}<button onclick={() => (confirming = true)} disabled={syncing}>Remove from library</button>{/if}
      </div>
      {#if syncing}<p class="meta">Editing and removing are paused while the HeckPod sync runs.</p>{/if}
      {#if confirming}
        <p class="confirm">
          Remove this album and delete its files from the library folder? Your original files aren't touched.
          <button class="danger" onclick={remove} disabled={removing || syncing}>Remove</button>
          <button onclick={() => (confirming = false)} disabled={removing}>Cancel</button>
        </p>
      {/if}
      {#if retryError}<p class="err">{retryError}</p>{/if}
      {#if removeError}<p class="err">{removeError}</p>{/if}
    </div>
  </header>
  <table>
    <tbody>
      {#each tracks as t, i (t.id)}
        <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
        <tr class:playing={nowPlayingId === t.id} ondblclick={(e) => { if (!inControl(e)) onPlay(items, i); }}>
          <td class="play"><button title="Play" onclick={() => onPlay(items, i)}>▶</button></td>
          <td class="num">{tracks.some((x) => x.disc > 1) ? `${t.disc}-` : ''}{t.number}</td>
          <td>{t.title}{#if t.artist !== album.album_artist}<span class="feat"> — {t.artist}</span>{/if}</td>
          <td class="fmt">{t.format.toUpperCase()}</td>
          <td class="dur">{formatDuration(t.duration_ms)}</td>
          <td class="add"><AddToPlaylist trackIds={[t.id]} {playlists} {onPlaylistsChanged} /></td>
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
  .err { margin: 8px 0 0; color: var(--danger); font-size: 13px; }
  .actions { display: flex; gap: 8px; flex-wrap: wrap; align-items: center; }
  .confirm { margin: 10px 0 0; display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
  .danger { background: var(--danger); color: #fff; border-color: var(--danger); }
  .add { width: 1%; white-space: nowrap; }
  table { width: 100%; border-collapse: collapse; }
  td { padding: 7px 8px; border-bottom: 1px solid var(--border); }
  .num, .dur, .fmt { color: var(--muted); width: 1%; white-space: nowrap; }
  .feat { color: var(--muted); }
  .play { width: 1%; }
  .play button { border: none; background: none; padding: 0 4px; color: var(--muted); }
  tr.playing td { color: var(--accent); font-weight: 600; }
</style>
