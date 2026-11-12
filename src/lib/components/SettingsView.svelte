<script lang="ts">
  import { onMount } from 'svelte';
  import { open } from '@tauri-apps/plugin-dialog';
  import { openUrl, revealItemInDir } from '@tauri-apps/plugin-opener';
  import * as api from '../api';
  import type { SettingsView } from '../types';

  let s = $state<SettingsView | null>(null);
  let key = $state('');
  let error = $state<string | null>(null);
  let saved = $state<string | null>(null);

  function show(v: SettingsView) {
    s = v;
    key = v.acoustid_key ?? '';
  }

  onMount(() => {
    api.getSettings().then(show).catch((e) => (error = String(e)));
  });

  async function run(action: () => Promise<SettingsView>, done: string) {
    error = null;
    saved = null;
    try {
      show(await action());
      saved = done;
    } catch (e) {
      error = String(e);
    }
  }

  async function changeFolder() {
    try {
      const picked = await open({ directory: true, title: 'Choose the HeckTunes library folder' });
      if (typeof picked === 'string') await run(() => api.setLibraryRoot(picked), 'Library folder saved.');
    } catch (e) {
      error = String(e);
    }
  }

  function saveKey(e: SubmitEvent) {
    e.preventDefault();
    run(() => api.setAcoustidKey(key.trim() || null), key.trim() ? 'AcoustID key saved.' : 'AcoustID key override removed.');
  }

  const fail = (e: unknown) => (error = String(e));
  const albumsNow = (n: number) => `${n} ${n === 1 ? 'album' : 'albums'}`;
</script>

<div class="settings">
  <h2>Settings</h2>
  {#if s?.library_unavailable}<p class="warn">{s.library_unavailable}</p>{/if}
  {#if s?.problem}
    <p class="warn">{s.problem}</p>
    <p class="meta">Saving here replaces settings.json; your library folder stays as it is.</p>
  {/if}
  {#if s?.restart_needed}<p class="note">Restart HeckTunes to use the saved settings.</p>{/if}
  {#if error}<p class="err">{error}</p>{/if}
  {#if saved}<p class="ok">{saved}</p>{/if}

  {#if s}
    <section>
      <h3>Library folder</h3>
      <p class="path">{s.library_root}</p>
      {#if s.saved_library_root && s.saved_library_root !== s.library_root}
        <p class="meta">After a restart: {s.saved_library_root}</p>
      {/if}
      <div class="row">
        <button onclick={() => revealItemInDir(s!.library_root).catch(fail)} disabled={!!s.library_unavailable}>Show in folder</button>
        <button onclick={changeFolder} disabled={s.album_count > 0}>Change…</button>
        {#if s.saved_library_root}
          <button onclick={() => run(() => api.setLibraryRoot(null), 'Back to the default folder.')} disabled={s.album_count > 0}>Use default</button>
        {/if}
      </div>
      {#if s.album_count > 0}
        <p class="meta">The folder can only be changed while the library is empty ({albumsNow(s.album_count)} now).</p>
      {/if}
    </section>

    <section>
      <h3>AcoustID key</h3>
      <p class="meta">
        Used to recognise albums that have no tags at all.
        {s.acoustid_key_active ? 'A key is in use.' : 'No key is in use, so untagged albums go to "Needs metadata".'}
        <button class="link" onclick={() => openUrl('https://acoustid.org/new-application').catch(fail)}>Get a free key</button>
      </p>
      <form class="row" onsubmit={saveKey}>
        <input bind:value={key} placeholder="Your AcoustID application key (optional)" spellcheck="false" autocomplete="off" />
        <button type="submit">Save</button>
      </form>
      <p class="meta">A new key is used after a restart.</p>
    </section>
  {/if}
</div>

<style>
  .settings { padding: 16px 24px; max-width: 760px; }
  h2 { margin: 0 0 12px; font-size: 24px; }
  h3 { margin: 0 0 8px; font-size: 16px; }
  section { background: var(--panel); border: 1px solid var(--border); border-radius: var(--radius); padding: 12px 16px; margin-bottom: 14px; }
  .path { font-family: ui-monospace, monospace; word-break: break-all; margin: 0 0 8px; }
  .row { display: flex; gap: 8px; flex-wrap: wrap; align-items: center; }
  .row input { flex: 1; min-width: 240px; }
  .meta { color: var(--muted); font-size: 13px; }
  .link { border: none; background: none; color: var(--accent); padding: 0; }
  .warn { color: var(--warn); }
  .note { color: var(--accent); }
  .err { color: var(--danger); }
  .ok { color: var(--text); font-weight: 600; }
</style>
