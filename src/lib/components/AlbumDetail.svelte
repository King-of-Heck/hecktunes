<script lang="ts">
  import { inControl } from '../rows';
  import { onMount } from 'svelte';
  import { albumTracks, cancelConvert, convertAlbum, convertUnavailable, coverUrl, deleteAlbum, formatDuration, removeAac, setAlbumSyncAs } from '../api';
  import type { AlbumRow, ConvertEvent, JobUpdate, PlaylistRow, QueueItem, SyncAs, TrackRow } from '../types';
  import AddToPlaylist from './AddToPlaylist.svelte';
  let {
    album,
    bust,
    jobs,
    playlists,
    retryJobId,
    syncing,
    converting,
    convert,
    nowPlayingId,
    onPlay,
    onRetry,
    onBack,
    onEdit,
    onPlaylistsChanged,
    onRemoved,
    onChanged,
  }: {
    album: AlbumRow;
    bust: number;
    jobs: Record<number, JobUpdate>;
    playlists: PlaylistRow[];
    retryJobId: number | null;
    /** A sync is copying library files: hold back changes to them until it ends. */
    syncing: boolean;
    /** Some album is being converted to AAC: library changes wait for it too. */
    converting: boolean;
    /** The latest Convert to AAC event for this album. */
    convert: ConvertEvent | null;
    nowPlayingId: number | null;
    onPlay: (items: QueueItem[], index: number) => void;
    onRetry: () => Promise<void>;
    onBack: () => void;
    onEdit: (tracks: TrackRow[]) => void;
    onPlaylistsChanged: () => Promise<void>;
    onRemoved: () => Promise<void>;
    /** The album row changed (sync choice, AAC copies removed): reload it. */
    onChanged: () => Promise<void>;
  } = $props();

  let tracks = $state<TrackRow[]>([]);

  const busy = $derived(syncing || converting);
  const busyNote = $derived(
    syncing ? 'Editing and removing are paused while the HeckPod sync runs.' : 'Editing and removing are paused while albums are converted to AAC.',
  );

  // Convert to AAC
  let unavailable = $state<string | null>(null);
  let aacError = $state<string | null>(null);
  let aacWorking = $state(false);
  let confirmingRemoveAac = $state(false);
  onMount(() => {
    convertUnavailable()
      .then((r) => (unavailable = r))
      .catch((e) => console.error('convert_unavailable failed', e));
  });
  const active = $derived(convert !== null && (convert.state === 'queued' || convert.state === 'converting'));
  const flacTracks = $derived(tracks.filter((t) => t.format.toLowerCase() === 'flac'));
  const missing = $derived(flacTracks.filter((t) => t.aac === null).length);
  const convertLabel = $derived(
    album.aac_count === 0 ? 'Convert to AAC' : missing === 1 ? 'Convert the last track to AAC' : `Convert the other ${missing} tracks to AAC`,
  );
  const failedHeading = (n: number) => (n === 1 ? "1 track couldn't be converted:" : `${n} tracks couldn't be converted:`);
  const cancelledNote = (n: number) => (n === 0 ? 'Conversion cancelled.' : `Conversion cancelled after ${n} ${n === 1 ? 'track' : 'tracks'}.`);

  async function aac(what: () => Promise<unknown>, reload: boolean) {
    aacError = null;
    aacWorking = true;
    try {
      await what();
      if (reload) await onChanged();
    } catch (e) {
      aacError = `${e}`;
    } finally {
      aacWorking = false;
    }
  }
  const startConvert = () => aac(() => convertAlbum(album.id), false);
  const stopConvert = () => aac(() => cancelConvert(album.id), false);
  function chooseSync(to: SyncAs) {
    if (to !== album.sync_as) aac(() => setAlbumSyncAs(album.id, to), true);
  }
  async function dropCopies() {
    await aac(() => removeAac(album.id), true);
    confirmingRemoveAac = false;
  }

  const items = $derived(tracks.map((t) => ({ id: t.id, title: t.title, artist: t.artist, album_title: album.title })));
  let retryError = $state<string | null>(null);
  let confirming = $state(false);
  let removing = $state(false);
  let removeError = $state<string | null>(null);

  async function remove() {
    removing = true;
    removeError = null;
    try {
      await deleteAlbum(album.id);
    } catch (e) {
      removeError = `Couldn't remove: ${e}`;
      removing = false;
      return;
    }
    await onRemoved().catch((e) => console.error('refresh after remove failed', e));
  }

  const terminal = (st: string) => st === 'done' || st === 'failed' || st === 'skipped';
  // Busy while our retry job exists and has not finished.
  const retrying = $derived(retryJobId !== null && jobs[retryJobId] !== undefined && !terminal(jobs[retryJobId].state));

  $effect(() => {
    // re-load whenever the album row (or bust counter) changes
    void bust;
    let cancelled = false;
    albumTracks(album.id)
      .then((t) => { if (!cancelled) tracks = t; })
      .catch((e) => console.error('album_tracks failed', e));
    return () => { cancelled = true; };
  });

  async function fix() {
    retryError = null;
    try {
      await onRetry();
    } catch (e) {
      console.error('retry_metadata failed', e);
      retryError = `Couldn't start lookup: ${e}`;
    }
  }
</script>

<div class="detail">
  <button class="back" onclick={onBack}>← Back</button>
  <header>
    {#if album.cover_path}<img src={coverUrl(album.cover_path, bust)} alt="" />{:else}<div class="noart">♪</div>{/if}
    <div>
      <h2>{album.title}</h2>
      <p class="artist">{album.album_artist}</p>
      <p class="meta">{[album.year, album.genre, `${album.track_count} tracks`].filter(Boolean).join(' · ')}</p>
      <div class="actions">
        <button onclick={() => onEdit(tracks)} disabled={busy}>Edit tags</button>
        <button onclick={fix} disabled={retrying || busy}>{retrying ? 'Looking up…' : 'Fix metadata'}</button>
        <AddToPlaylist trackIds={tracks.map((t) => t.id)} {playlists} {onPlaylistsChanged} label="Add to playlist" />
        {#if !confirming}<button onclick={() => (confirming = true)} disabled={busy}>Remove from library</button>{/if}
      </div>
      {#if busy}<p class="meta">{busyNote}</p>{/if}
      {#if flacTracks.length > 0}
        <div class="aac">
          {#if active && convert}
            <span class="progress">
              {#if convert.state === 'converting'}Converting to AAC… {convert.done} of {convert.total}{:else}Waiting to convert to AAC…{/if}
            </span>
            <button onclick={stopConvert} disabled={aacWorking}>Cancel</button>
          {:else}
            {#if album.aac_count > 0}
              <span class="label">Echo Mini gets</span>
              <span class="seg" role="group" aria-label="Files to sync">
                <button class:on={album.sync_as === 'flac'} aria-pressed={album.sync_as === 'flac'} onclick={() => chooseSync('flac')} disabled={aacWorking}>FLAC</button>
                <button class:on={album.sync_as === 'aac'} aria-pressed={album.sync_as === 'aac'} onclick={() => chooseSync('aac')} disabled={aacWorking}>AAC</button>
              </span>
            {/if}
            {#if missing > 0}
              <button onclick={startConvert} disabled={aacWorking || syncing || unavailable !== null}>{convertLabel}</button>
            {/if}
            {#if album.aac_count > 0 && !confirmingRemoveAac}
              <button onclick={() => (confirmingRemoveAac = true)} disabled={aacWorking || busy}>Remove AAC copies</button>
            {/if}
          {/if}
        </div>
        {#if album.aac_count > 0 && !active}
          <p class="meta">{album.aac_count} of {flacTracks.length} tracks have a 256 kbps AAC copy beside the FLAC.</p>
        {:else if !active && missing > 0 && unavailable === null}
          <p class="meta">Makes 256 kbps AAC (.m4a) copies beside the FLAC files, for a smaller Echo Mini sync. The FLAC files stay.</p>
        {/if}
        {#if confirmingRemoveAac}
          <p class="confirm">
            Delete this album's AAC copies? The FLAC files stay, and the Echo Mini gets them again at the next sync.
            <button class="danger" onclick={dropCopies} disabled={aacWorking || busy}>Delete copies</button>
            <button onclick={() => (confirmingRemoveAac = false)} disabled={aacWorking}>Cancel</button>
          </p>
        {/if}
        {#if unavailable !== null && missing > 0}<p class="meta">{unavailable}</p>{/if}
        {#if convert?.state === 'finished' && convert.failed.length > 0}
          <div class="err">
            {failedHeading(convert.failed.length)}
            <ul>{#each convert.failed as f}<li>{f.title}: {f.error}</li>{/each}</ul>
          </div>
        {:else if convert?.state === 'cancelled'}
          <p class="meta">{cancelledNote(convert.converted)}</p>
        {:else if convert?.state === 'failed'}
          <p class="err">Couldn't convert: {convert.error}</p>
        {/if}
        {#if aacError}<p class="err">{aacError}</p>{/if}
      {/if}
      {#if confirming}
        <p class="confirm">
          Remove this album and delete its files from the library folder? Your original files aren't touched.
          <button class="danger" onclick={remove} disabled={removing || busy}>Remove</button>
          <button onclick={() => (confirming = false)} disabled={removing}>Cancel</button>
        </p>
      {/if}
      {#if retryError}<p class="err">{retryError}</p>{/if}
      {#if removeError}<p class="err">{removeError}</p>{/if}
    </div>
  </header>
  <table>
    <tbody>
      {#each tracks as t, i (t.id)}
        <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
        <tr class:playing={nowPlayingId === t.id} ondblclick={(e) => { if (!inControl(e)) onPlay(items, i); }}>
          <td class="play"><button title="Play" onclick={() => onPlay(items, i)}>▶</button></td>
          <td class="num">{tracks.some((x) => x.disc > 1) ? `${t.disc}-` : ''}{t.number}</td>
          <td>{t.title}{#if t.artist !== album.album_artist}<span class="feat"> — {t.artist}</span>{/if}</td>
          <td class="fmt">{t.format.toUpperCase()}{t.aac ? ' + AAC' : ''}</td>
          <td class="dur">{formatDuration(t.duration_ms)}</td>
          <td class="add"><AddToPlaylist trackIds={[t.id]} {playlists} {onPlaylistsChanged} /></td>
        </tr>
      {/each}
    </tbody>
  </table>
</div>

<style>
  .detail { padding: 16px 24px; overflow: auto; height: 100%; }
  .back { border: none; background: none; color: var(--accent); padding: 0; margin-bottom: 12px; }
  header { display: flex; gap: 20px; align-items: flex-end; margin-bottom: 20px; }
  img, .noart { width: 180px; height: 180px; border-radius: var(--radius); object-fit: cover; background: var(--panel); }
  .noart { display: grid; place-items: center; font-size: 48px; color: var(--muted); }
  h2 { margin: 0 0 4px; font-size: 24px; }
  .artist { margin: 0; font-size: 16px; color: var(--accent); }
  .meta { margin: 4px 0 12px; color: var(--muted); }
  .err { margin: 8px 0 0; color: var(--danger); font-size: 13px; }
  .actions { display: flex; gap: 8px; flex-wrap: wrap; align-items: center; }
  .confirm { margin: 10px 0 0; display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
  .danger { background: var(--danger); color: #fff; border-color: var(--danger); }
  .add { width: 1%; white-space: nowrap; }
  .aac { display: flex; gap: 8px; flex-wrap: wrap; align-items: center; margin-top: 10px; }
  .aac .label, .progress { color: var(--muted); }
  .seg { display: inline-flex; }
  .seg button { border-radius: 0; }
  .seg button:first-child { border-radius: var(--radius) 0 0 var(--radius); }
  .seg button:last-child { border-radius: 0 var(--radius) var(--radius) 0; margin-left: -1px; }
  .seg button.on { background: var(--accent); color: #fff; border-color: var(--accent); }
  .err ul { margin: 4px 0 0; padding-left: 18px; }
  table { width: 100%; border-collapse: collapse; }
  td { padding: 7px 8px; border-bottom: 1px solid var(--border); }
  .num, .dur, .fmt { color: var(--muted); width: 1%; white-space: nowrap; }
  .feat { color: var(--muted); }
  .play { width: 1%; }
  .play button { border: none; background: none; padding: 0 4px; color: var(--muted); }
  tr.playing td { color: var(--accent); font-weight: 600; }
</style>
