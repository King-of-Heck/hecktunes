<script lang="ts">
  import { formatDuration, resolveJob, vinylPreview } from '../api';
  import type { JobChoice, JobUpdate, PlanTrack } from '../types';
  import { BIG_NUDGE_MS, NUDGE_MS, formatCutTime, nudgeEnd, nudgeStart, removeCut, retitle, splitAt } from '../vinylCuts';
  let { job, syncing }: { job: JobUpdate & { state: 'needs_vinyl_review' }; syncing: boolean } = $props();

  let tracks = $state<PlanTrack[]>([]);
  let busy = $state(false);
  let picking = $state(false);
  // svelte-ignore state_referenced_locally
  let query = $state(job.label);
  let splitting = $state<{ index: number; at: number } | null>(null);
  let listenError = $state<string | null>(null);
  let resolveError = $state<string | null>(null);
  // The plan's track list as last loaded, so a reply that keeps it (e.g. a search) keeps the user's edits.
  let lastPlanTracks = '';

  // A new plan (after Change release, a search or a refused import) arrives as a new `job` for the same id.
  $effect(() => {
    const planTracks = JSON.stringify(job.plan.tracks);
    if (planTracks !== lastPlanTracks) {
      lastPlanTracks = planTracks;
      tracks = job.plan.tracks.map((t) => ({ ...t }));
      splitting = null;
    }
    busy = false;
  });

  const release = $derived(job.plan.release);
  const mismatch = $derived(release && tracks.length !== release.tracks.length ? `${tracks.length} pieces, but this release has ${release.tracks.length} tracks.` : null);
  const lastOnFile = (i: number) => i === tracks.length - 1 || tracks[i + 1].file !== tracks[i].file;
  const step = (e: MouseEvent) => (e.shiftKey ? BIG_NUDGE_MS : NUDGE_MS);

  async function choose(choice: JobChoice) {
    busy = true;
    resolveError = null;
    try {
      await resolveJob(job.job_id, choice);
    } catch (e) {
      console.error('resolve_job failed', e);
      resolveError = String(e);
      busy = false;
    }
  }

  function listen(file: number, atMs: number) {
    listenError = null;
    vinylPreview(job.plan.files[file].path, atMs).catch((e) => (listenError = String(e)));
  }

  function edit(next: PlanTrack[]) {
    tracks = retitle(next, release);
    // An open Split panel points at a row and time from before this edit.
    splitting = null;
  }
</script>

<div class="backdrop">
  <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="vinyl-title">
    <h3 id="vinyl-title">Check the tracks in “{job.label}”</h3>

    <div class="release">
      {#if release}
        <img src={`https://coverartarchive.org/release/${release.mbid}/front-250`} alt="" onerror={(e) => ((e.currentTarget as HTMLImageElement).style.visibility = 'hidden')} />
        <div class="info">
          <strong>{release.artist} – {release.title}</strong>
          <span class="muted">{[release.date, release.country, release.format, `${release.tracks.length} tracks`].filter(Boolean).join(' · ')}</span>
        </div>
      {:else}
        <div class="info"><strong>No release matched</strong>{#if job.plan.note}<span class="muted">{job.plan.note}</span>{/if}</div>
      {/if}
      <button onclick={() => (picking = !picking)} disabled={busy}>{picking ? 'Hide releases' : 'Change release'}</button>
    </div>

    {#if picking}
      <div class="picker">
        <form class="search" onsubmit={(e) => { e.preventDefault(); if (query.trim()) choose({ kind: 'search', query: query.trim() }); }}>
          <input bind:value={query} placeholder="Search MusicBrainz (artist album)" />
          <button type="submit" disabled={busy}>Search</button>
        </form>
        <ul>
          {#each job.plan.candidates as c (c.mbid)}
            <li>
              <span>{c.artist} – {c.title} <span class="muted">{[c.date, c.country, c.format, `${c.tracks.length} tracks`].filter(Boolean).join(' · ')}</span></span>
              <button onclick={() => { picking = false; choose({ kind: 'vinyl_realign', mbid: c.mbid }); }} disabled={busy || c.mbid === release?.mbid}>Use this</button>
            </li>
          {:else}
            <li class="muted">No releases found. Try a different search.</li>
          {/each}
        </ul>
      </div>
    {/if}

    {#if job.plan.warning}<p class="warn">{job.plan.warning}</p>{/if}

    <table>
      <tbody>
        {#each tracks as t, i (`${t.file}-${t.start_ms}`)}
          {#if i === 0 || tracks[i - 1].file !== t.file}
            {#if job.plan.files.length > 1}<tr class="file"><td colspan="5">{job.plan.files[t.file].name}</td></tr>{/if}
          {/if}
          <tr>
            <td class="num">{i + 1}</td>
            <td class="title">
              {t.title}
              {#if t.cut === 'estimated'}<span class="badge" title="No gap here: the cut was placed from the track lengths. Give it a listen.">check this cut</span>{/if}
            </td>
            <td class="time">
              <button title="Earlier (Shift: 5 s)" onclick={(e) => edit(nudgeStart(tracks, i, -step(e)))}>◀</button>
              {formatCutTime(t.start_ms)}
              <button title="Later (Shift: 5 s)" onclick={(e) => edit(nudgeStart(tracks, i, step(e)))}>▶</button>
              <button class="link" onclick={() => listen(t.file, t.start_ms)}>Listen</button>
            </td>
            <td class="muted">{formatDuration(t.end_ms - t.start_ms)}</td>
            <td class="actions">
              <button class="link" onclick={() => (splitting = { index: i, at: Math.round((t.start_ms + t.end_ms) / 2) })}>Split…</button>
              {#if i > 0 && tracks[i - 1].file === t.file}<button class="link" onclick={() => edit(removeCut(tracks, i))}>Remove cut</button>{/if}
            </td>
          </tr>
          {#if splitting && splitting.index === i}
            {@const s = splitting}
            <tr class="split">
              <td></td>
              <td colspan="4">
                New cut at
                <button onclick={(e) => (splitting = { index: i, at: s.at - step(e) })}>◀</button>
                {formatCutTime(s.at)}
                <button onclick={(e) => (splitting = { index: i, at: s.at + step(e) })}>▶</button>
                <button class="link" onclick={() => listen(t.file, s.at)}>Listen</button>
                <button onclick={() => { edit(splitAt(tracks, i, s.at)); splitting = null; }}>Add cut</button>
                <button onclick={() => (splitting = null)}>Cancel</button>
              </td>
            </tr>
          {/if}
          {#if lastOnFile(i)}
            <tr class="end">
              <td></td>
              <td class="muted">End of {job.plan.files.length > 1 ? job.plan.files[t.file].name : 'recording'}</td>
              <td class="time">
                <button title="Earlier (Shift: 5 s)" onclick={(e) => edit(nudgeEnd(tracks, i, -step(e), job.plan.files[t.file].duration_ms))}>◀</button>
                {formatCutTime(t.end_ms)}
                <button title="Later (Shift: 5 s)" onclick={(e) => edit(nudgeEnd(tracks, i, step(e), job.plan.files[t.file].duration_ms))}>▶</button>
                <button class="link" onclick={() => listen(t.file, t.end_ms)}>Listen</button>
              </td>
              <td colspan="2"></td>
            </tr>
          {/if}
        {/each}
      </tbody>
    </table>

    {#if listenError}<p class="warn">{listenError}</p>{/if}
    {#if resolveError}<p class="warn">{resolveError}</p>{/if}
    {#if mismatch}<p class="warn">{mismatch}</p>{/if}
    {#if syncing}<p class="muted">Waiting for the HeckPod sync to finish before changing your library.</p>{/if}
    <div class="footer">
      <button onclick={() => choose({ kind: 'skip' })} disabled={busy}>Don't import</button>
      <button onclick={() => choose({ kind: 'vinyl_confirm', tracks, matched: false })} disabled={busy || syncing}>Import without a match</button>
      <button class="primary" onclick={() => choose({ kind: 'vinyl_confirm', tracks, matched: true })} disabled={busy || syncing || !release || mismatch !== null}>Import</button>
    </div>
  </div>
</div>

<style>
  .backdrop { position: fixed; inset: 0; background: rgb(0 0 0 / 0.45); display: grid; place-items: center; z-index: 60; }
  .dialog { background: var(--panel); border-radius: 12px; width: min(820px, 94vw); max-height: 88vh; overflow: auto; padding: 20px 24px; }
  h3 { margin: 0 0 12px; }
  .release { display: flex; gap: 12px; align-items: center; margin-bottom: 12px; }
  .release img { width: 64px; height: 64px; border-radius: 6px; object-fit: cover; background: var(--bg); }
  .info { flex: 1; display: flex; flex-direction: column; gap: 2px; min-width: 0; }
  .picker { border: 1px solid var(--border); border-radius: 8px; padding: 10px; margin-bottom: 12px; }
  .search { display: flex; gap: 8px; margin-bottom: 8px; }
  .picker ul { list-style: none; margin: 0; padding: 0; }
  .picker li { display: flex; justify-content: space-between; gap: 8px; padding: 6px 0; border-bottom: 1px solid var(--border); }
  table { width: 100%; border-collapse: collapse; font-size: 13px; }
  td { padding: 4px 6px; border-bottom: 1px solid var(--border); vertical-align: middle; }
  .num { color: var(--muted); width: 2em; text-align: right; }
  .time { white-space: nowrap; font-variant-numeric: tabular-nums; }
  .time button, .split button { padding: 0 6px; }
  .actions { white-space: nowrap; text-align: right; }
  tr.file td { font-weight: 600; padding-top: 12px; }
  tr.split td, tr.end td { background: var(--bg); }
  .badge { margin-left: 6px; padding: 1px 6px; border-radius: 8px; background: var(--warn); color: #fff; font-size: 11px; }
  .muted { color: var(--muted); font-size: 12px; }
  .warn { color: var(--warn); white-space: pre-line; }
  .link { border: none; background: none; color: var(--accent); padding: 0 4px; }
  .footer { display: flex; justify-content: flex-end; gap: 8px; margin-top: 16px; }
</style>
