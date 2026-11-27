<script lang="ts">
  import { open } from '@tauri-apps/plugin-dialog';
  import { coverUrl, setAlbumCover, updateAlbum, updateTrack } from '../api';
  import type { AlbumRow, TrackRow } from '../types';
  let { album, tracks, syncing, onClose }: { album: AlbumRow; tracks: TrackRow[]; syncing: boolean; onClose: (changed: boolean) => void } = $props();

  // Initial values only: the editor is mounted fresh for each edit session.
  // svelte-ignore state_referenced_locally
  let a = $state({ title: album.title, album_artist: album.album_artist, year: album.year ?? '', genre: album.genre ?? '' });
  // svelte-ignore state_referenced_locally
  let ts = $state(tracks.map((t) => ({ id: t.id, title: t.title, artist: t.artist, disc: t.disc, number: t.number })));
  let saving = $state(false);
  let error = $state<string | null>(null);
  let coverBust = $state(0);
  // svelte-ignore state_referenced_locally
  let coverPath = $state(album.cover_path);
  let coverChanged = $state(false);
  let coverStatus = $state<string | null>(null);

  async function changeCover() {
    error = null;
    coverStatus = null;
    try {
      const picked = await open({ multiple: false, filters: [{ name: 'Images', extensions: ['jpg', 'jpeg', 'png'] }] });
      if (typeof picked !== 'string') return;
      saving = true;
      await setAlbumCover(album.id, picked);
      coverChanged = true;
      coverPath = album.cover_path ?? `${album.folder_path}/cover.jpg`;
      coverBust++;
      coverStatus = 'Cover updated';
    } catch (e) {
      error = String(e);
    } finally {
      saving = false;
    }
  }

  async function save() {
    saving = true;
    error = null;
    try {
      await updateAlbum(album.id, { title: a.title, album_artist: a.album_artist, year: a.year || null, genre: a.genre || null });
      for (const t of ts) {
        const orig = tracks.find((o) => o.id === t.id)!;
        if (orig.title !== t.title || orig.artist !== t.artist || orig.disc !== t.disc || orig.number !== t.number) {
          await updateTrack(t.id, { title: t.title, artist: t.artist, disc: Number(t.disc), number: Number(t.number) });
        }
      }
      onClose(true);
    } catch (e) {
      error = String(e);
      saving = false;
    }
  }
</script>

<div class="backdrop">
  <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="editor-title">
    <h3 id="editor-title">Edit tags</h3>
    <div class="cover-row">
      {#if coverUrl(coverPath, coverBust)}<img class="cover" src={coverUrl(coverPath, coverBust)} alt="Album cover" />{:else}<div class="cover none">No cover</div>{/if}
      <div>
        <button onclick={changeCover} disabled={saving || syncing}>Change cover…</button>
        {#if coverStatus}<p class="note">{coverStatus}</p>{/if}
      </div>
    </div>
    <div class="album">
      <label>Album <input bind:value={a.title} /></label>
      <label>Album artist <input bind:value={a.album_artist} /></label>
      <label>Year <input bind:value={a.year} /></label>
      <label>Genre <input bind:value={a.genre} /></label>
    </div>
    <table>
      <thead><tr><th>Disc</th><th>#</th><th>Title</th><th>Artist</th></tr></thead>
      <tbody>
        {#each ts as t (t.id)}
          <tr>
            <td class="n"><input type="number" min="1" bind:value={t.disc} /></td>
            <td class="n"><input type="number" min="1" bind:value={t.number} /></td>
            <td><input bind:value={t.title} /></td>
            <td><input bind:value={t.artist} /></td>
          </tr>
        {/each}
      </tbody>
    </table>
    {#if error}<p class="error">{error}</p>{/if}
    {#if syncing}<p class="note">Saving is paused while the HeckPod sync or an AAC conversion runs.</p>{/if}
    <div class="footer">
      <button onclick={() => onClose(coverChanged)} disabled={saving}>Cancel</button>
      <button class="primary" onclick={save} disabled={saving || syncing}>{saving ? 'Saving…' : 'Save'}</button>
    </div>
  </div>
</div>

<style>
  .backdrop { position: fixed; inset: 0; background: rgb(0 0 0 / 0.45); display: grid; place-items: center; z-index: 60; }
  .dialog { background: var(--panel); border-radius: 12px; width: min(820px, 94vw); max-height: 88vh; overflow: auto; padding: 20px 24px; }
  h3 { margin: 0 0 12px; }
  .cover-row { display: flex; align-items: center; gap: 14px; margin-bottom: 14px; }
  .cover { width: 96px; height: 96px; object-fit: cover; border-radius: 6px; background: var(--bg, #222); }
  .cover.none { display: grid; place-items: center; font-size: 12px; color: var(--muted); border: 1px dashed var(--muted); }
  .album { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; margin-bottom: 16px; }
  label { display: flex; flex-direction: column; gap: 4px; font-size: 12px; color: var(--muted); }
  table { width: 100%; border-collapse: collapse; }
  th { text-align: left; font-size: 12px; color: var(--muted); font-weight: 500; padding: 4px; }
  td { padding: 3px 4px; }
  td.n { width: 64px; }
  .error { color: var(--danger); }
  .note { color: var(--muted); font-size: 13px; }
  .footer { display: flex; justify-content: flex-end; gap: 8px; margin-top: 16px; }
</style>
