<script lang="ts">
  import { inControl } from '../rows';
  import { onMount } from 'svelte';
  import * as api from '../api';
  import { formatDuration } from '../api';
  import type { PlaylistEntry, PlaylistRow, QueueItem } from '../types';
  let {
    playlist,
    nowPlayingId,
    onPlay,
    onChanged,
    onDeleted,
    onOpenAlbum,
  }: {
    playlist: PlaylistRow;
    nowPlayingId: number | null;
    onPlay: (items: QueueItem[], index: number) => void;
    onChanged: () => Promise<void>;
    onDeleted: () => Promise<void>;
    onOpenAlbum: (albumId: number) => void;
  } = $props();

  let entries = $state<PlaylistEntry[]>([]);
  let error = $state<string | null>(null);
  let busy = $state(false);
  let renaming = $state(false);
  let name = $state('');
  let confirmDelete = $state(false);

  const items = $derived(entries.map((e) => ({ id: e.id, title: e.title, artist: e.artist, album_title: e.album_title })));
  const total = $derived(entries.reduce((sum, e) => sum + e.duration_ms, 0));

  /** Reloads the entries; false (with `error` set) when that failed. */
  async function load(): Promise<boolean> {
    try {
      entries = await api.playlistEntries(playlist.id);
      return true;
    } catch (e) {
      error = String(e);
      return false;
    }
  }
  onMount(load);

  /** Runs a change, then reloads this page and the sidebar counts. */
  async function change(action: () => Promise<unknown>): Promise<boolean> {
    busy = true;
    error = null;
    try {
      await action();
      const reloaded = await load();
      await onChanged();
      return reloaded;
    } catch (e) {
      error = String(e);
      return false;
    } finally {
      busy = false;
    }
  }

  const move = (e: PlaylistEntry, to: number) => change(() => api.movePlaylistItem(playlist.id, e.item_id, to));
  const removeEntry = (e: PlaylistEntry) => change(() => api.removePlaylistItems(playlist.id, [e.item_id]));

  async function rename(ev: SubmitEvent) {
    ev.preventDefault();
    if (await change(() => api.renamePlaylist(playlist.id, name))) renaming = false;
  }

  async function remove() {
    busy = true;
    error = null;
    try {
      await api.deletePlaylist(playlist.id);
      await onDeleted();
    } catch (e) {
      error = String(e);
      busy = false;
    }
  }
</script>

<div class="playlist">
  <header>
    {#if renaming}
      <form onsubmit={rename}>
        <!-- svelte-ignore a11y_autofocus -->
        <input bind:value={name} autofocus />
        <button type="submit" disabled={busy}>Save</button>
        <button type="button" onclick={() => (renaming = false)}>Cancel</button>
      </form>
    {:else}
      <h2>{playlist.name}</h2>
    {/if}
    <p class="meta">{entries.length} {entries.length === 1 ? 'song' : 'songs'} · {formatDuration(total)}</p>
    <div class="actions">
      {#if !renaming}<button onclick={() => { name = playlist.name; renaming = true; }}>Rename</button>{/if}
      {#if confirmDelete}
        <span>Delete this playlist? Its songs stay in your library.</span>
        <button class="danger" onclick={remove} disabled={busy}>Delete</button>
        <button onclick={() => (confirmDelete = false)} disabled={busy}>Cancel</button>
      {:else}
        <button onclick={() => (confirmDelete = true)}>Delete playlist</button>
      {/if}
    </div>
  </header>
  {#if error}<p class="err">{error}</p>{/if}
  {#if entries.length === 0}
    <p class="empty">This playlist is empty. Use the + button next to any song, or "Add to playlist" on an album.</p>
  {:else}
    <table>
      <tbody>
        {#each entries as e, i (e.item_id)}
          <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
          <tr class:playing={nowPlayingId === e.id} ondblclick={(e) => { if (!inControl(e)) onPlay(items, i); }}>
            <td class="num"><button class="playbtn" title="Play" onclick={() => onPlay(items, i)}>▶</button>{i + 1}</td>
            <td>{e.title}<span class="sub"> — {e.artist}</span></td>
            <td><button class="link" onclick={() => onOpenAlbum(e.album_id)}>{e.album_title}</button></td>
            <td class="dur">{formatDuration(e.duration_ms)}</td>
            <td class="ctl">
              <button title="Move up" disabled={busy || i === 0} onclick={() => move(e, i - 1)}>▲</button>
              <button title="Move down" disabled={busy || i === entries.length - 1} onclick={() => move(e, i + 1)}>▼</button>
              <button title="Remove from playlist" disabled={busy} onclick={() => removeEntry(e)}>✕</button>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</div>

<style>
  .playlist { padding: 16px 24px; }
  header { margin-bottom: 16px; }
  h2 { margin: 0 0 4px; font-size: 24px; }
  form { display: flex; gap: 8px; }
  .meta { margin: 4px 0 10px; color: var(--muted); }
  .actions { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
  .danger { background: var(--danger); color: #fff; border-color: var(--danger); }
  .err { color: var(--danger); }
  .empty { color: var(--muted); }
  table { width: 100%; border-collapse: collapse; }
  td { padding: 6px 8px; border-bottom: 1px solid var(--border); }
  .num, .dur, .ctl { width: 1%; white-space: nowrap; color: var(--muted); }
  .ctl button { padding: 2px 8px; }
  .sub { color: var(--muted); }
  .playbtn { border: none; background: none; padding: 0 4px; color: var(--muted); }
  tr.playing td { color: var(--accent); font-weight: 600; }
  .link { border: none; background: none; padding: 0; color: var(--accent); text-align: left; }
</style>
