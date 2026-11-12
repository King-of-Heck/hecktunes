<script lang="ts">
  import { onMount } from 'svelte';
  import * as api from './lib/api';
  import { reducePlayerEvent } from './lib/playerQueue';
  import type { AlbumRow, JobUpdate, PlayerStatus, PlaylistRow, QueueItem, SyncEvent, TrackRow, View } from './lib/types';
  import NowPlaying from './lib/components/NowPlaying.svelte';
  import SyncPanel from './lib/components/SyncPanel.svelte';
  import ReleasePicker from './lib/components/ReleasePicker.svelte';
  import TagEditor from './lib/components/TagEditor.svelte';
  import Sidebar from './lib/components/Sidebar.svelte';
  import AlbumGrid from './lib/components/AlbumGrid.svelte';
  import AlbumDetail from './lib/components/AlbumDetail.svelte';
  import ArtistList from './lib/components/ArtistList.svelte';
  import SongsView from './lib/components/SongsView.svelte';
  import PlaylistView from './lib/components/PlaylistView.svelte';
  import DropOverlay from './lib/components/DropOverlay.svelte';
  import ImportTray from './lib/components/ImportTray.svelte';
  import AboutView from './lib/components/AboutView.svelte';
  import SettingsPage from './lib/components/SettingsView.svelte';

  let view = $state<View>({ kind: 'albums' });
  let albums = $state<AlbumRow[]>([]);
  let playlists = $state<PlaylistRow[]>([]);
  let selectedId = $state<number | null>(null);
  let hovering = $state(false);
  let jobs = $state<Record<number, JobUpdate>>({});
  let bust = $state(0);
  // bumped after imports/edits so the Songs view reloads
  let songsVersion = $state(0);
  // album id -> id of its running Fix-metadata job; lives here so it survives AlbumDetail remounts.
  let retryJobs = $state<Record<number, number>>({});
  let syncEvent = $state<SyncEvent | null>(null);
  // Set when the library folder was missing at startup (e.g. its drive isn't connected).
  let libraryUnavailable = $state<string | null>(null);
  const syncing = $derived(syncEvent !== null && syncEvent.state !== 'finished' && syncEvent.state !== 'failed');

  const needs = $derived(albums.filter((a) => a.needs_metadata));
  const artistName = $derived(view.kind === 'artists' ? (view.artist ?? null) : null);
  const playlistId = $derived(view.kind === 'playlist' ? view.id : null);
  const currentPlaylist = $derived(playlists.find((p) => p.id === playlistId) ?? null);
  const gridAlbums = $derived(
    view.kind === 'needs_metadata' ? needs : artistName !== null ? albums.filter((a) => a.album_artist === artistName) : albums,
  );
  $effect(() => {
    if (artistName !== null && albums.length > 0 && !albums.some((a) => a.album_artist === artistName)) view = { kind: 'artists' };
  });
  const selected = $derived(albums.find((a) => a.id === selectedId) ?? null);
  const jobList = $derived(Object.values(jobs).sort((a, b) => b.job_id - a.job_id));

  let editing = $state<TrackRow[] | null>(null);
  const waiting = $derived(
    jobList
      .filter((j): j is JobUpdate & ({ state: 'needs_choice' } | { state: 'duplicate' }) => j.state === 'needs_choice' || j.state === 'duplicate')
      .sort((a, b) => a.job_id - b.job_id)[0] ?? null,
  );

  async function refresh() {
    albums = await api.listAlbums();
    bust++;
    songsVersion++;
  }

  async function refreshPlaylists() {
    playlists = await api.listPlaylists();
  }

  function select(v: View) {
    view = v;
    selectedId = null;
  }

  async function createPlaylist(name: string) {
    const id = await api.createPlaylist(name);
    await refreshPlaylists();
    select({ kind: 'playlist', id });
  }

  async function onJob(u: JobUpdate) {
    jobs[u.job_id] = u;
    if (u.state === 'done') {
      await refresh();
      await refreshPlaylists();
      // A re-identified album gets a new id: follow it if it was open.
      if (selectedId !== null && !albums.some((a) => a.id === selectedId)) selectedId = u.album_id;
      // The old album is gone, so its retry entry is finished with.
      for (const [albumId, jobId] of Object.entries(retryJobs)) if (jobId === u.job_id) delete retryJobs[Number(albumId)];
    }
  }

  async function retry(albumId: number) {
    retryJobs[albumId] = await api.retryMetadata(albumId);
  }

  function clearFinished() {
    for (const j of Object.values(jobs)) {
      if (j.state === 'done' || j.state === 'skipped' || j.state === 'failed') delete jobs[j.job_id];
    }
  }

  const logError = (what: string) => (e: unknown) => console.error(what, e);

  let queue = $state<QueueItem[]>([]);
  let queueIndex = $state(0);
  let playerStatus = $state<PlayerStatus | null>(null);
  const nowPlaying = $derived(queue[queueIndex] ?? null);
  const nowPlayingId = $derived(playerStatus && playerStatus.state !== 'idle' ? playerStatus.track_id : null);

  // Track a play was requested for but whose playing/error status hasn't arrived yet.
  let pendingId: number | null = null;
  let playError = $state<string | null>(null);

  function playFrom(items: QueueItem[], index: number) {
    queue = items;
    queueIndex = index;
    const id = items[index].id;
    pendingId = id;
    playError = null;
    api.playTrack(id).catch((e) => {
      console.error('play failed', e);
      if (pendingId === id) pendingId = null;
      playError = String(e);
    });
  }

  function next() {
    if (queueIndex + 1 < queue.length) playFrom(queue, queueIndex + 1);
  }

  function previous() {
    if ((playerStatus?.position_ms ?? 0) > 3000 || queueIndex === 0) api.playerSeek(0).catch(logError('seek failed'));
    else playFrom(queue, queueIndex - 1);
  }

  function onPlayer(s: PlayerStatus) {
    playerStatus = s;
    const r = reducePlayerEvent(s, queue, queueIndex, pendingId);
    pendingId = r.pendingId;
    if (s.state === 'playing' || s.state === 'error') playError = null;
    if (r.action === 'advance') next();
    else if (r.action === 'stop') api.playerStop().catch(logError('stop failed'));
    else if (r.action === 'clear') {
      queue = [];
      playError = null;
    }
  }

  onMount(() => {
    refresh().catch(logError('refresh failed'));
    refreshPlaylists().catch(logError('playlists failed'));
    api
      .getSettings()
      .then((s) => (libraryUnavailable = s.library_unavailable))
      .catch(logError('settings failed'));
    const unlisten = [
      api.onImportJob(onJob),
      api.onSync((e) => (syncEvent = e)),
      api.onPlayer(onPlayer),
      api.onFileDrop({
        hover: (on) => (hovering = on),
        drop: (paths) => api.importPaths(paths).catch(logError('import failed')),
      }),
    ];
    return () => unlisten.forEach((p) => p.then((off) => off()));
  });
</script>

<div class="app">
<div class="layout">
  <Sidebar {view} needsCount={needs.length} {playlists} {syncing} onSelect={select} onCreatePlaylist={createPlaylist} />
  <main>
    {#if libraryUnavailable}
      <p class="banner">{libraryUnavailable} <button class="link" onclick={() => select({ kind: 'settings' })}>Settings</button></p>
    {/if}
    {#if selected}
      {#key selected.id}
        <AlbumDetail
          album={selected}
          {bust}
          {jobs}
          {playlists}
          retryJobId={retryJobs[selected.id] ?? null}
          {syncing}
          {nowPlayingId}
          onPlay={playFrom}
          onRetry={() => retry(selected.id)}
          onBack={() => (selectedId = null)}
          onEdit={(t) => (editing = t)}
          onPlaylistsChanged={refreshPlaylists}
          onRemoved={async () => {
            selectedId = null;
            await refresh();
            await refreshPlaylists();
          }}
        />
      {/key}
    {:else if view.kind === 'artists' && artistName === null}
      <ArtistList {albums} {bust} onOpen={(artist) => (view = { kind: 'artists', artist })} />
    {:else if view.kind === 'songs'}
      <SongsView version={songsVersion} {playlists} {nowPlayingId} onPlay={playFrom} onOpenAlbum={(id) => (selectedId = id)} onPlaylistsChanged={refreshPlaylists} />
    {:else if view.kind === 'playlist'}
      {#if currentPlaylist}
        {#key currentPlaylist.id}
          <PlaylistView
            playlist={currentPlaylist}
            {nowPlayingId}
            onPlay={playFrom}
            onChanged={refreshPlaylists}
            onDeleted={async () => {
              select({ kind: 'albums' });
              await refreshPlaylists();
            }}
            onOpenAlbum={(id) => (selectedId = id)}
          />
        {/key}
      {/if}
    {:else if view.kind === 'device'}
      <SyncPanel {albums} {playlists} {syncEvent} onClearResult={() => (syncEvent = null)} />
    {:else if view.kind === 'settings'}
      <SettingsPage />
    {:else if view.kind === 'about'}
      <AboutView />
    {:else}
      {#if artistName !== null}
        <div class="crumbs">
          <button class="back" onclick={() => (view = { kind: 'artists' })}>← Artists</button>
          <h2>{artistName}</h2>
        </div>
      {/if}
      <AlbumGrid albums={gridAlbums} {bust} onOpen={(a) => (selectedId = a.id)} />
    {/if}
  </main>
</div>
  <NowPlaying status={playerStatus} item={nowPlaying} {playError} hasNext={queueIndex + 1 < queue.length} onPrevious={previous} onNext={next} />
</div>
<DropOverlay visible={hovering} />
<ImportTray jobs={jobList} raised={nowPlaying !== null} onClear={clearFinished} />
{#if waiting}
  {#key waiting.job_id}
    <ReleasePicker job={waiting} {syncing} />
  {/key}
{/if}
{#if editing && selected}
  <TagEditor album={selected} tracks={editing} {syncing} onClose={(changed) => { editing = null; if (changed) refresh().catch(logError('refresh failed')); }} />
{/if}

<style>
  .app { display: grid; grid-template-rows: minmax(0, 1fr) auto; height: 100vh; }
  .layout { display: grid; grid-template-columns: 210px 1fr; min-height: 0; }
  main { overflow: auto; }
  .crumbs { padding: 16px 24px 0; }
  .crumbs h2 { margin: 4px 0 0; font-size: 22px; }
  .back { border: none; background: none; color: var(--accent); padding: 0; }
  .banner { margin: 0; padding: 8px 24px; background: var(--panel); border-bottom: 1px solid var(--border); color: var(--warn); }
  .link { border: none; background: none; color: var(--accent); padding: 0; }
</style>
