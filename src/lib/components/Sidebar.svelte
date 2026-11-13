<script lang="ts">
  import type { PlaylistRow, View } from '../types';
  let {
    view,
    needsCount,
    playlists,
    syncing,
    onSelect,
    onCreatePlaylist,
    onImportVinyl,
  }: {
    view: View;
    needsCount: number;
    playlists: PlaylistRow[];
    syncing: boolean;
    onSelect: (v: View) => void;
    onCreatePlaylist: (name: string) => Promise<void>;
    onImportVinyl: () => void;
  } = $props();

  let naming = $state(false);
  let name = $state('');
  let error = $state<string | null>(null);
  const is = (k: View['kind']) => view.kind === k;

  async function create(e: SubmitEvent) {
    e.preventDefault();
    error = null;
    try {
      await onCreatePlaylist(name);
      naming = false;
      name = '';
    } catch (err) {
      error = String(err);
    }
  }
</script>

<nav>
  <h1>HeckTunes</h1>
  <p class="section">Library</p>
  <button class:active={is('albums')} onclick={() => onSelect({ kind: 'albums' })}>Albums</button>
  <button class:active={is('artists')} onclick={() => onSelect({ kind: 'artists' })}>Artists</button>
  <button class:active={is('songs')} onclick={() => onSelect({ kind: 'songs' })}>Songs</button>
  <button class:active={is('needs_metadata')} onclick={() => onSelect({ kind: 'needs_metadata' })}>
    Needs metadata {#if needsCount > 0}<span class="badge">{needsCount}</span>{/if}
  </button>
  <button class="new" onclick={onImportVinyl}>+ Import vinyl recording…</button>

  <p class="section">Playlists</p>
  {#each playlists as p (p.id)}
    <button class:active={view.kind === 'playlist' && view.id === p.id} onclick={() => onSelect({ kind: 'playlist', id: p.id })}>
      <span class="name">{p.name}</span><span class="count">{p.track_count}</span>
    </button>
  {/each}
  {#if naming}
    <form onsubmit={create}>
      <!-- svelte-ignore a11y_autofocus -->
      <input bind:value={name} placeholder="Playlist name" autofocus onkeydown={(e) => { if (e.key === 'Escape') { naming = false; name = ''; error = null; } }} />
    </form>
    {#if error}<p class="err">{error}</p>{/if}
  {:else}
    <button class="new" onclick={() => { naming = true; error = null; }}>+ New playlist</button>
  {/if}

  <p class="section">Device</p>
  <button class:active={is('device')} onclick={() => onSelect({ kind: 'device' })}>
    Echo Mini {#if syncing}<span class="badge sync">syncing</span>{/if}
  </button>
  <div class="spacer"></div>
  <button class:active={is('settings')} onclick={() => onSelect({ kind: 'settings' })}>Settings</button>
  <button class:active={is('about')} onclick={() => onSelect({ kind: 'about' })}>About</button>
</nav>

<style>
  nav { background: var(--sidebar); border-right: 1px solid var(--border); padding: 16px 10px; display: flex; flex-direction: column; gap: 2px; overflow-y: auto; }
  h1 { font-size: 18px; margin: 0 8px 16px; color: var(--accent); }
  .section { font-size: 11px; text-transform: uppercase; letter-spacing: 0.06em; color: var(--muted); margin: 14px 8px 4px; }
  button { border: none; background: transparent; text-align: left; display: flex; justify-content: space-between; align-items: center; gap: 6px; }
  button.active { background: var(--panel); font-weight: 600; }
  .name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .count { color: var(--muted); font-size: 12px; }
  .new { color: var(--muted); }
  form { padding: 2px 4px; }
  input { width: 100%; }
  .err { margin: 2px 8px; color: var(--danger); font-size: 12px; }
  .badge { background: var(--warn); color: #fff; border-radius: 10px; padding: 0 7px; font-size: 11px; }
  .spacer { flex: 1; }
  .badge.sync { background: var(--accent); }
</style>
