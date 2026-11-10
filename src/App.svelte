<script lang="ts">
  import { onMount } from 'svelte';
  import * as api from './lib/api';
  import type { AlbumRow, JobUpdate, View } from './lib/types';
  import Sidebar from './lib/components/Sidebar.svelte';
  import AlbumGrid from './lib/components/AlbumGrid.svelte';
  import AlbumDetail from './lib/components/AlbumDetail.svelte';
  import DropOverlay from './lib/components/DropOverlay.svelte';
  import ImportTray from './lib/components/ImportTray.svelte';

  let view = $state<View>('albums');
  let albums = $state<AlbumRow[]>([]);
  let selectedId = $state<number | null>(null);
  let hovering = $state(false);
  let jobs = $state<Record<number, JobUpdate>>({});
  let bust = $state(0);

  const visible = $derived(view === 'albums' ? albums : albums.filter((a) => a.needs_metadata));
  const selected = $derived(albums.find((a) => a.id === selectedId) ?? null);
  const jobList = $derived(Object.values(jobs).sort((a, b) => b.job_id - a.job_id));

  async function refresh() {
    albums = await api.listAlbums();
    bust++;
  }

  async function onJob(u: JobUpdate) {
    jobs[u.job_id] = u;
    if (u.state === 'done') {
      await refresh();
      // A re-identified album gets a new id: follow it if it was open.
      if (selectedId !== null && !albums.some((a) => a.id === selectedId)) selectedId = u.album_id;
    }
  }

  function clearFinished() {
    for (const j of Object.values(jobs)) {
      if (j.state === 'done' || j.state === 'skipped' || j.state === 'failed') delete jobs[j.job_id];
    }
  }

  onMount(() => {
    refresh();
    const unlisten = [
      api.onImportJob(onJob),
      api.onFileDrop({ hover: (on) => (hovering = on), drop: (paths) => api.importPaths(paths) }),
    ];
    return () => unlisten.forEach((p) => p.then((off) => off()));
  });
</script>

<div class="layout">
  <Sidebar {view} needsCount={albums.filter((a) => a.needs_metadata).length} onSelect={(v) => { view = v; selectedId = null; }} />
  <main>
    {#if selected}
      <AlbumDetail album={selected} {bust} onBack={() => (selectedId = null)} onEdit={() => {}} />
    {:else}
      <AlbumGrid albums={visible} {bust} onOpen={(a) => (selectedId = a.id)} />
    {/if}
  </main>
</div>
<DropOverlay visible={hovering} />
<ImportTray jobs={jobList} onClear={clearFinished} />

<style>
  .layout { display: grid; grid-template-columns: 210px 1fr; height: 100vh; }
  main { overflow: auto; }
</style>
