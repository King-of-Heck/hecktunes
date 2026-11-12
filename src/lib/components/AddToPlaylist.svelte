<script lang="ts">
  import { addToPlaylist, createPlaylist } from '../api';
  import type { PlaylistRow } from '../types';
  let {
    trackIds,
    playlists,
    onPlaylistsChanged,
    label = '+',
  }: { trackIds: number[]; playlists: PlaylistRow[]; onPlaylistsChanged: () => Promise<void>; label?: string } = $props();

  let open = $state(false);
  let root = $state<HTMLDetailsElement | null>(null);
  function onWindowClick(e: MouseEvent) {
    if (open && root && !root.contains(e.target as Node)) open = false;
  }
  let name = $state('');
  let error = $state<string | null>(null);

  async function addTo(id: number) {
    error = null;
    try {
      await addToPlaylist(id, trackIds);
      await onPlaylistsChanged();
      open = false;
    } catch (e) {
      error = String(e);
    }
  }

  async function createAndAdd(e: SubmitEvent) {
    e.preventDefault();
    error = null;
    try {
      const id = await createPlaylist(name);
      await addToPlaylist(id, trackIds);
      name = '';
      await onPlaylistsChanged();
      open = false;
    } catch (err) {
      error = String(err);
    }
  }
</script>

<svelte:window onclick={onWindowClick} onkeydown={(e) => e.key === 'Escape' && (open = false)} />

<details class="menu" bind:open bind:this={root}>
  <summary title="Add to playlist">{label}</summary>
  <div class="pop">
    <p class="head">Add to playlist</p>
    {#each playlists as p (p.id)}
      <button onclick={() => addTo(p.id)}>{p.name}</button>
    {/each}
    <form onsubmit={createAndAdd}>
      <input bind:value={name} placeholder="New playlist…" />
    </form>
    {#if error}<p class="err">{error}</p>{/if}
  </div>
</details>

<style>
  .menu { position: relative; display: inline-block; }
  summary { list-style: none; cursor: pointer; border: 1px solid var(--border); border-radius: 6px; padding: 3px 10px; background: var(--panel); }
  summary::-webkit-details-marker { display: none; }
  .pop { position: absolute; right: 0; top: calc(100% + 4px); z-index: 30; min-width: 220px; background: var(--panel); border: 1px solid var(--border); border-radius: var(--radius); padding: 6px; display: flex; flex-direction: column; gap: 2px; box-shadow: 0 6px 20px rgba(0, 0, 0, 0.18); }
  .head { margin: 2px 6px 6px; font-size: 11px; text-transform: uppercase; letter-spacing: 0.06em; color: var(--muted); }
  .pop button { border: none; background: transparent; text-align: left; }
  .pop button:hover { background: var(--sidebar); }
  input { width: 100%; margin-top: 4px; }
  .err { margin: 6px; color: var(--danger); font-size: 12px; }
</style>
