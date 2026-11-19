<script lang="ts">
  import { onMount } from 'svelte';
  import * as api from '../api';
  import { coverUrl, formatBytes } from '../api';
  import type { AlbumRow, PlanSummary, PlaylistRow, Selection, SelectionKind, SyncEvent, Volume } from '../types';
  let {
    albums,
    playlists,
    bust = 0,
    syncEvent,
    onClearResult,
  }: {
    albums: AlbumRow[];
    playlists: PlaylistRow[];
    bust?: number;
    syncEvent: SyncEvent | null;
    onClearResult: () => void;
  } = $props();

  let volumes = $state<Volume[]>([]);
  let card = $state<string | null>(null);
  let selection = $state<Selection>({ albums: [], artists: [], playlists: [] });
  let summary = $state<PlanSummary | null>(null);
  let previewError = $state<string | null>(null);
  let volumeError = $state<string | null>(null);
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
      .map(([name, list]) => ({
        name,
        albums: [...list].sort((x, y) => (x.year ?? '9999').localeCompare(y.year ?? '9999') || x.title.localeCompare(y.title)),
        cover: list.find((a) => a.cover_path)?.cover_path ?? null,
      }));
  });
  // How many of an artist's albums will be on the card.
  const onCard = (a: { name: string; albums: AlbumRow[] }) =>
    hasArtist(a.name) ? a.albums.length : a.albums.filter((al) => hasAlbum(al.id)).length;

  const hasAlbum = (id: number) => selection.albums.includes(id);
  const hasArtist = (name: string) => selection.artists.includes(name);
  const hasPlaylist = (id: number) => selection.playlists.includes(id);

  async function loadVolumes() {
    volumeError = null;
    try {
      volumes = await api.listVolumes();
      if (card === null || !volumes.some((v) => v.root === card)) card = volumes.find((v) => v.hecktunes_card)?.root ?? null;
    } catch (e) {
      volumeError = String(e);
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
  <h2>HeckPod</h2>

  <section>
    <div class="row">
      <select bind:value={card} disabled={running}>
        <option value={null}>Choose the HeckPod's microSD card…</option>
        {#each volumes as v (v.root)}
          <option value={v.root} disabled={v.player_internal}>
            {v.label || 'Untitled'} ({v.root}) · {formatBytes(v.available)} free{v.hecktunes_card ? ' · synced before' : ''}{v.player_internal ? " · player's own storage" : ''}
          </option>
        {/each}
      </select>
      <button onclick={loadVolumes} disabled={running}>Refresh</button>
    </div>
    {#if volumeError}<p class="err">{volumeError}</p>{/if}
    {#if volumes.length === 0}
      <p class="meta">No cards found. Put the microSD card in a card reader (or connect the HeckPod), then press Refresh.</p>
    {/if}
    {#if selectedVolume?.last_synced}
      <p class="meta">Last synced {new Date(selectedVolume.last_synced * 1000).toLocaleString()}</p>
    {/if}
  </section>

  <section>
    <h3>What goes on the HeckPod</h3>
    {#if playlists.length > 0}
      <h4>Playlists</h4>
      <div class="list">
        {#each playlists as p (p.id)}
          <label class="item">
            <input type="checkbox" checked={hasPlaylist(p.id)} disabled={running} onchange={(e) => toggle('playlist', String(p.id), e.currentTarget.checked)} />
            <div class="thumb square noart">♫</div>
            <div class="text">
              <span class="name">{p.name}</span>
              <span class="sub">{p.track_count} {p.track_count === 1 ? 'song' : 'songs'}</span>
            </div>
          </label>
        {/each}
      </div>
    {/if}
    <h4>Artists and albums</h4>
    <div class="list">
      {#each artists as a (a.name)}
        {@const whole = hasArtist(a.name)}
        {@const n = onCard(a)}
        <div class="artist" class:open={expanded[a.name]}>
          <div class="item">
            <input
              type="checkbox"
              checked={whole}
              indeterminate={!whole && n > 0}
              disabled={running}
              aria-label={`All of ${a.name}`}
              onchange={(e) => toggle('artist', a.name, e.currentTarget.checked)}
            />
            <button class="expand" onclick={() => (expanded[a.name] = !expanded[a.name])} aria-expanded={!!expanded[a.name]}>
              {#if a.cover}<img class="thumb round" src={coverUrl(a.cover, bust)} alt="" loading="lazy" />{:else}<div class="thumb round noart">♪</div>{/if}
              <div class="text">
                <span class="name">{a.name}</span>
                <span class="sub">
                  {a.albums.length} {a.albums.length === 1 ? 'album' : 'albums'}{whole ? ' · all, including future albums' : n > 0 ? ` · ${n} selected` : ''}
                </span>
              </div>
              <span class="chev" aria-hidden="true">›</span>
            </button>
          </div>
          {#if expanded[a.name]}
            <div class="albums">
              {#each a.albums as al (al.id)}
                <label class="album" class:on={whole || hasAlbum(al.id)} class:locked={whole}>
                  {#if al.cover_path}<img src={coverUrl(al.cover_path, bust)} alt="" loading="lazy" />{:else}<div class="noart">♪</div>{/if}
                  <div class="text">
                    <span class="name">{al.title}</span>
                    <span class="sub">{[al.year, `${al.track_count} ${al.track_count === 1 ? 'song' : 'songs'}`].filter(Boolean).join(' · ')}</span>
                  </div>
                  <input
                    type="checkbox"
                    checked={whole || hasAlbum(al.id)}
                    disabled={running || whole}
                    title={whole ? `Included because all of ${a.name} is ticked` : undefined}
                    onchange={(e) => toggle('album', String(al.id), e.currentTarget.checked)}
                  />
                </label>
              {/each}
            </div>
          {/if}
        </div>
      {:else}
        <p class="meta">Your library is empty.</p>
      {/each}
    </div>
  </section>

  {#if previewError || actionError || (summary && !running)}
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
  {/if}

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
          <p class="ok">The player's library is ready, so the HeckPod won't need to rescan the card.</p>
        {:else if echo?.status === 'failed' && echo.code === null && echo.log.startsWith('cancelled')}
          <p class="warn">You stopped echo-sync. The HeckPod will rescan the card on its own.</p>
        {:else if echo?.status === 'failed' && echo.code === null && echo.log.startsWith('timed out')}
          <p class="warn">echo-sync took too long and was stopped. The HeckPod will rescan the card on its own.</p>
          <pre>{echo.log}</pre>
        {:else if echo?.status === 'failed'}
          <p class="warn">echo-sync failed (exit code {echo.code ?? '?'}). The HeckPod will rescan the card on its own.</p>
          <pre>{echo.log}</pre>
        {:else if echo?.status === 'missing'}
          <p class="warn">echo-sync didn't run: {echo.reason}. The HeckPod will rescan the card on its own.</p>
        {/if}
        {#if !o.cancelled}<p class="meta">Eject the card safely before removing it.</p>{/if}
      {:else if syncEvent.state === 'failed'}
        <p class="err">Sync failed: {syncEvent.error}</p>
      {/if}
    </section>
  {/if}
</div>

<style>
  .sync { padding: 16px 24px; }
  h2 { margin: 0 0 12px; font-size: 24px; }
  h3 { margin: 0 0 8px; font-size: 16px; }
  h4 { margin: 12px 0 4px; font-size: 12px; text-transform: uppercase; letter-spacing: 0.06em; color: var(--muted); }
  section { background: var(--panel); border: 1px solid var(--border); border-radius: var(--radius); padding: 12px 16px; margin-bottom: 14px; }
  .row { display: flex; gap: 8px; }
  select { flex: 1; min-width: 0; }
  .list { display: flex; flex-direction: column; }
  .item { display: flex; align-items: center; gap: 12px; padding: 6px 4px; border-bottom: 1px solid var(--border); }
  .list > .item:last-child, .list > .artist:last-child > .item { border-bottom: none; }
  .artist.open > .item { border-bottom: none; }
  .expand {
    flex: 1; min-width: 0; display: flex; align-items: center; gap: 12px;
    border: none; background: none; padding: 2px 4px; border-radius: 6px; text-align: left;
  }
  .expand:hover { background: var(--bg); }
  .thumb { width: 40px; height: 40px; flex: none; object-fit: cover; background: var(--bg); }
  .thumb.round { border-radius: 50%; }
  .thumb.square { border-radius: 6px; }
  .noart { display: grid; place-items: center; color: var(--muted); background: var(--bg); }
  .text { flex: 1; min-width: 0; display: flex; flex-direction: column; }
  .name { font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .chev { color: var(--muted); font-size: 20px; line-height: 1; transition: transform 0.15s; }
  .open .chev { transform: rotate(90deg); }
  .albums {
    display: grid; grid-template-columns: repeat(auto-fill, minmax(240px, 1fr)); gap: 8px;
    padding: 4px 4px 12px 30px; border-bottom: 1px solid var(--border);
  }
  .artist:last-child .albums { border-bottom: none; }
  .album {
    display: flex; align-items: center; gap: 10px; padding: 6px; cursor: pointer;
    border: 1px solid var(--border); border-radius: var(--radius); background: var(--panel);
  }
  .album:hover { border-color: var(--muted); }
  .album.on { border-color: var(--accent); }
  .album.locked { cursor: default; }
  .album img, .album .noart { width: 52px; height: 52px; flex: none; border-radius: 6px; object-fit: cover; }
  .album .name { font-weight: 500; }
  .sub, .meta { color: var(--muted); font-size: 13px; }
  .err { color: var(--danger); }
  .warn { color: var(--warn); }
  .ok { color: var(--text); font-weight: 600; }
  .primary { background: var(--accent); color: var(--accent-text); border-color: var(--accent); padding: 6px 20px; }
  progress { width: 100%; margin: 6px 0; }
  pre { white-space: pre-wrap; font-size: 12px; background: var(--bg); padding: 8px; border-radius: 6px; max-height: 200px; overflow: auto; }
</style>
