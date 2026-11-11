<script lang="ts">
  import { onMount } from 'svelte';
  import * as api from '../api';
  import { formatBytes } from '../api';
  import type { AlbumRow, PlanSummary, PlaylistRow, Selection, SelectionKind, SyncEvent, Volume } from '../types';
  let {
    albums,
    playlists,
    syncEvent,
    onClearResult,
  }: { albums: AlbumRow[]; playlists: PlaylistRow[]; syncEvent: SyncEvent | null; onClearResult: () => void } = $props();

  let volumes = $state<Volume[]>([]);
  let card = $state<string | null>(null);
  let selection = $state<Selection>({ albums: [], artists: [], playlists: [] });
  let summary = $state<PlanSummary | null>(null);
  let previewError = $state<string | null>(null);
  let actionError = $state<string | null>(null);
  let expanded = $state<Record<string, boolean>>({});
  let previewSeq = 0;
  // Sync was pressed and its first event hasn't arrived yet.
  let starting = $state(false);

  const running = $derived(syncEvent !== null && syncEvent.state !== 'finished' && syncEvent.state !== 'failed');
  const selectedVolume = $derived(volumes.find((v) => v.root === card) ?? null);
  const artists = $derived.by(() => {
    const map = new Map<string, AlbumRow[]>();
    for (const a of albums) map.set(a.album_artist, [...(map.get(a.album_artist) ?? []), a]);
    return [...map.entries()]
      .sort(([x], [y]) => x.localeCompare(y, undefined, { sensitivity: 'base' }))
      .map(([name, list]) => ({ name, albums: list }));
  });

  const hasAlbum = (id: number) => selection.albums.includes(id);
  const hasArtist = (name: string) => selection.artists.includes(name);
  const hasPlaylist = (id: number) => selection.playlists.includes(id);

  async function loadVolumes() {
    try {
      volumes = await api.listVolumes();
      if (card === null || !volumes.some((v) => v.root === card)) card = volumes.find((v) => v.hecktunes_card)?.root ?? null;
    } catch (e) {
      previewError = String(e);
    }
  }

  onMount(() => {
    loadVolumes();
    api.getSelection().then((s) => (selection = s)).catch((e) => (previewError = String(e)));
  });

  // Re-plan whenever the card or the ticks change, and again when a sync ends.
  $effect(() => {
    const c = card;
    void selection;
    // Bump first, so a preview still in flight can't land after this point.
    const seq = ++previewSeq;
    if (c === null || running) {
      summary = null;
      return;
    }
    previewError = null;
    api
      .previewSync(c)
      .then((s) => {
        if (seq === previewSeq) summary = s;
      })
      .catch((e) => {
        if (seq === previewSeq) {
          summary = null;
          previewError = String(e);
        }
      });
  });

  $effect(() => {
    if (syncEvent !== null) starting = false;
  });

  // A finished sync changes "last synced" and free space.
  $effect(() => {
    if (syncEvent?.state === 'finished') loadVolumes();
  });

  async function toggle(kind: SelectionKind, reference: string, on: boolean) {
    actionError = null;
    try {
      await api.setSelected(kind, reference, on);
      selection = await api.getSelection();
    } catch (e) {
      actionError = String(e);
    }
  }

  async function sync() {
    if (card === null || starting) return;
    actionError = null;
    starting = true;
    onClearResult();
    try {
      await api.startSync(card);
    } catch (e) {
      starting = false;
      actionError = String(e);
    }
  }

  async function cancel() {
    actionError = null;
    try {
      await api.cancelSync();
    } catch (e) {
      actionError = `Couldn't cancel: ${e}`;
    }
  }
</script>

<div class="sync">
  <h2>Echo Mini</h2>

  <section>
    <div class="row">
      <select bind:value={card} disabled={running}>
        <option value={null}>Choose the Echo Mini's microSD card…</option>
        {#each volumes as v (v.root)}
          <option value={v.root} disabled={v.player_internal}>
            {v.label || 'Untitled'} ({v.root}) · {formatBytes(v.available)} free{v.hecktunes_card ? ' · synced before' : ''}{v.player_internal ? " · player's own storage" : ''}
          </option>
        {/each}
      </select>
      <button onclick={loadVolumes} disabled={running}>Refresh</button>
    </div>
    {#if volumes.length === 0}
      <p class="meta">No cards found. Put the microSD card in a card reader (or connect the Echo Mini), then press Refresh.</p>
    {/if}
    {#if selectedVolume?.last_synced}
      <p class="meta">Last synced {new Date(selectedVolume.last_synced * 1000).toLocaleString()}</p>
    {/if}
  </section>

  <section>
    <h3>What goes on the Echo Mini</h3>
    {#if playlists.length > 0}
      <h4>Playlists</h4>
      {#each playlists as p (p.id)}
        <label class="tick">
          <input type="checkbox" checked={hasPlaylist(p.id)} disabled={running} onchange={(e) => toggle('playlist', String(p.id), e.currentTarget.checked)} />
          {p.name} <span class="sub">{p.track_count} songs</span>
        </label>
      {/each}
    {/if}
    <h4>Artists and albums</h4>
    {#each artists as a (a.name)}
      <div class="artist">
        <label class="tick">
          <input type="checkbox" checked={hasArtist(a.name)} disabled={running} onchange={(e) => toggle('artist', a.name, e.currentTarget.checked)} />
          <strong>{a.name}</strong>
        </label>
        <button class="twisty" onclick={() => (expanded[a.name] = !expanded[a.name])}>
          {expanded[a.name] ? 'Hide albums' : `${a.albums.length} ${a.albums.length === 1 ? 'album' : 'albums'}`}
        </button>
        {#if expanded[a.name]}
          {#each a.albums as al (al.id)}
            <label class="tick album">
              <input
                type="checkbox"
                checked={hasArtist(a.name) || hasAlbum(al.id)}
                disabled={running || hasArtist(a.name)}
                onchange={(e) => toggle('album', String(al.id), e.currentTarget.checked)}
              />
              {al.title}{al.year ? ` (${al.year})` : ''}
            </label>
          {/each}
        {/if}
      </div>
    {:else}
      <p class="meta">Your library is empty.</p>
    {/each}
  </section>

  <section>
    {#if previewError}<p class="err">{previewError}</p>{/if}
    {#if summary && !running}
      <p>
        {summary.first_sync ? 'First sync to this card. ' : ''}Copy {summary.add} new and {summary.replace} changed
        {summary.add + summary.replace === 1 ? 'file' : 'files'} ({formatBytes(summary.bytes_to_copy)}), remove {summary.remove},
        {summary.unchanged} already up to date.
      </p>
      {#if summary.available !== null}
        <p class="meta">Needs {formatBytes(summary.space_needed)} · {formatBytes(summary.available)} free</p>
      {/if}
      {#if !summary.fits}<p class="err">Not enough space on the card. Untick something or free up space first.</p>{/if}
      {#each summary.warnings as w}<p class="warn">{w}</p>{/each}
      <button class="primary" onclick={sync} disabled={!summary.fits || starting}>{starting ? 'Starting…' : 'Sync'}</button>
    {/if}
    {#if actionError}<p class="err">{actionError}</p>{/if}
  </section>

  {#if syncEvent}
    <section class="status">
      {#if syncEvent.state === 'preparing'}
        <p>Getting ready…</p>
      {:else if syncEvent.state === 'progress'}
        <p>{syncEvent.phase === 'removing' ? 'Removing' : 'Copying'} {syncEvent.done + 1} of {syncEvent.total}: {syncEvent.current}</p>
        <progress max={syncEvent.bytes_total || syncEvent.total || 1} value={syncEvent.bytes_total ? syncEvent.bytes_done : syncEvent.done}></progress>
        <button onclick={cancel}>Cancel</button>
      {:else if syncEvent.state === 'echo_sync'}
        <p>Building the player's library (echo-sync)… this can take a minute.</p>
        <button onclick={cancel}>Cancel</button>
      {:else if syncEvent.state === 'finished'}
        {@const o = syncEvent.outcome}
        {@const echo = syncEvent.echo_sync}
        <p class="ok">
          {o.cancelled ? 'Sync cancelled.' : 'Sync finished.'} Copied {o.copied}, removed {o.removed}{o.adopted ? `, kept ${o.adopted} already there` : ''}.
        </p>
        {#each o.conflicts as c}<p class="warn">Left alone: {c} (HeckTunes didn't put a file there, or its record of it looked wrong).</p>{/each}
        {#each o.warnings as w}<p class="warn">{w}</p>{/each}
        {#each syncEvent.warnings as w}<p class="warn">{w}</p>{/each}
        {#if echo?.status === 'done'}
          <p class="ok">The player's library is ready, so the Echo Mini won't need to rescan the card.</p>
        {:else if echo?.status === 'failed'}
          <p class="warn">echo-sync failed (exit code {echo.code ?? '?'}). The Echo Mini will rescan the card on its own.</p>
          <pre>{echo.log}</pre>
        {:else if echo?.status === 'missing'}
          <p class="warn">echo-sync didn't run: {echo.reason}. The Echo Mini will rescan the card on its own.</p>
        {/if}
        {#if !o.cancelled}<p class="meta">Eject the card safely before removing it.</p>{/if}
      {:else if syncEvent.state === 'failed'}
        <p class="err">Sync failed: {syncEvent.error}</p>
      {/if}
    </section>
  {/if}
</div>

<style>
  .sync { padding: 16px 24px; max-width: 820px; }
  h2 { margin: 0 0 12px; font-size: 24px; }
  h3 { margin: 0 0 8px; font-size: 16px; }
  h4 { margin: 12px 0 4px; font-size: 12px; text-transform: uppercase; letter-spacing: 0.06em; color: var(--muted); }
  section { background: var(--panel); border: 1px solid var(--border); border-radius: var(--radius); padding: 12px 16px; margin-bottom: 14px; }
  .row { display: flex; gap: 8px; }
  select { flex: 1; min-width: 0; }
  .tick { display: flex; gap: 8px; align-items: center; padding: 3px 0; }
  .tick.album { padding-left: 26px; }
  .artist { border-bottom: 1px solid var(--border); padding: 4px 0; }
  .twisty { border: none; background: none; color: var(--muted); padding: 0 0 0 26px; font-size: 12px; }
  .sub, .meta { color: var(--muted); font-size: 13px; }
  .err { color: var(--danger); }
  .warn { color: var(--warn); }
  .ok { color: var(--text); font-weight: 600; }
  .primary { background: var(--accent); color: var(--accent-text); border-color: var(--accent); padding: 6px 20px; }
  progress { width: 100%; margin: 6px 0; }
  pre { white-space: pre-wrap; font-size: 12px; background: var(--bg); padding: 8px; border-radius: 6px; max-height: 200px; overflow: auto; }
</style>
