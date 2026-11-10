<script lang="ts">
  import { coverUrl } from '../api';
  import type { AlbumRow } from '../types';
  let { albums, bust, onOpen }: { albums: AlbumRow[]; bust: number; onOpen: (a: AlbumRow) => void } = $props();
</script>

{#if albums.length === 0}
  <div class="empty">
    <p>Drag an album folder anywhere onto this window to add it.</p>
  </div>
{:else}
  <div class="grid">
    {#each albums as a (a.id)}
      <button class="card" onclick={() => onOpen(a)}>
        {#if a.cover_path}
          <img src={coverUrl(a.cover_path, bust)} alt="" loading="lazy" />
        {:else}
          <div class="noart">♪</div>
        {/if}
        <span class="title">{a.title}</span>
        <span class="artist">{a.album_artist}{a.year ? ` · ${a.year}` : ''}</span>
        {#if a.needs_metadata}<span class="flag">Needs metadata</span>{/if}
      </button>
    {/each}
  </div>
{/if}

<style>
  .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(160px, 1fr)); gap: 20px; padding: 20px; }
  .card { border: none; background: none; padding: 0; text-align: left; display: flex; flex-direction: column; gap: 3px; }
  img, .noart { width: 100%; aspect-ratio: 1; border-radius: var(--radius); object-fit: cover; background: var(--panel); box-shadow: 0 1px 4px rgb(0 0 0 / 0.15); }
  .noart { display: grid; place-items: center; font-size: 40px; color: var(--muted); }
  .title { font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; margin-top: 4px; }
  .artist { color: var(--muted); font-size: 12px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .flag { color: var(--warn); font-size: 11px; }
  .empty { height: 100%; display: grid; place-items: center; color: var(--muted); font-size: 16px; }
</style>
