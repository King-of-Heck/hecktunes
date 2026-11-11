<script lang="ts">
  import { coverUrl } from '../api';
  import type { AlbumRow } from '../types';
  let { albums, bust, onOpen }: { albums: AlbumRow[]; bust: number; onOpen: (artist: string) => void } = $props();

  const artists = $derived.by(() => {
    const map = new Map<string, AlbumRow[]>();
    for (const a of albums) map.set(a.album_artist, [...(map.get(a.album_artist) ?? []), a]);
    return [...map.entries()]
      .sort(([x], [y]) => x.localeCompare(y, undefined, { sensitivity: 'base' }))
      .map(([name, list]) => ({ name, count: list.length, cover: list.find((a) => a.cover_path)?.cover_path ?? null }));
  });
</script>

<div class="grid">
  {#each artists as a (a.name)}
    <button class="artist" onclick={() => onOpen(a.name)}>
      {#if a.cover}<img src={coverUrl(a.cover, bust)} alt="" />{:else}<div class="noart">♪</div>{/if}
      <span class="name">{a.name}</span>
      <span class="count">{a.count} {a.count === 1 ? 'album' : 'albums'}</span>
    </button>
  {:else}
    <p class="empty">No artists yet. Drop an album anywhere in the window to import it.</p>
  {/each}
</div>

<style>
  .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(160px, 1fr)); gap: 18px; padding: 20px 24px; }
  .artist { border: none; background: none; padding: 0; text-align: left; display: flex; flex-direction: column; gap: 4px; }
  img, .noart { width: 100%; aspect-ratio: 1; border-radius: 50%; object-fit: cover; background: var(--panel); }
  .noart { display: grid; place-items: center; font-size: 40px; color: var(--muted); }
  .name { font-weight: 600; text-align: center; }
  .count { color: var(--muted); font-size: 12px; text-align: center; }
  .empty { color: var(--muted); }
</style>
