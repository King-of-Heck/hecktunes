<script lang="ts">
  import { formatDuration, listSongs } from '../api';
  import type { PlaylistRow, SongRow } from '../types';
  import AddToPlaylist from './AddToPlaylist.svelte';
  let {
    version,
    playlists,
    onOpenAlbum,
    onPlaylistsChanged,
  }: { version: number; playlists: PlaylistRow[]; onOpenAlbum: (albumId: number) => void; onPlaylistsChanged: () => Promise<void> } = $props();

  let songs = $state<SongRow[]>([]);
  let filter = $state('');
  let error = $state<string | null>(null);

  $effect(() => {
    void version; // reload after imports and edits
    let cancelled = false;
    listSongs()
      .then((s) => {
        if (!cancelled) songs = s;
      })
      .catch((e) => (error = String(e)));
    return () => {
      cancelled = true;
    };
  });

  const shown = $derived.by(() => {
    const q = filter.trim().toLowerCase();
    if (!q) return songs;
    return songs.filter((s) => [s.title, s.artist, s.album_title, s.album_artist].some((f) => f.toLowerCase().includes(q)));
  });
</script>

<div class="songs">
  <header>
    <h2>Songs</h2>
    <input type="search" placeholder="Filter" bind:value={filter} />
    <span class="count">{shown.length} songs</span>
  </header>
  {#if error}<p class="err">{error}</p>{/if}
  <table>
    <thead><tr><th>Title</th><th>Artist</th><th>Album</th><th class="dur">Time</th><th></th></tr></thead>
    <tbody>
      {#each shown as s (s.id)}
        <tr>
          <td>{s.title}</td>
          <td>{s.artist}</td>
          <td><button class="link" onclick={() => onOpenAlbum(s.album_id)}>{s.album_title}</button></td>
          <td class="dur">{formatDuration(s.duration_ms)}</td>
          <td class="add"><AddToPlaylist trackIds={[s.id]} {playlists} {onPlaylistsChanged} /></td>
        </tr>
      {/each}
    </tbody>
  </table>
</div>

<style>
  .songs { padding: 16px 24px; }
  header { display: flex; align-items: center; gap: 12px; margin-bottom: 12px; }
  h2 { margin: 0; font-size: 22px; }
  .count { color: var(--muted); }
  table { width: 100%; border-collapse: collapse; }
  th { text-align: left; font-weight: 600; color: var(--muted); font-size: 12px; padding: 6px 8px; border-bottom: 1px solid var(--border); }
  td { padding: 6px 8px; border-bottom: 1px solid var(--border); }
  .dur, .add { width: 1%; white-space: nowrap; color: var(--muted); }
  .link { border: none; background: none; padding: 0; color: var(--accent); text-align: left; }
  .err { color: var(--danger); }
</style>
