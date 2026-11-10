<script lang="ts">
  import { formatDuration, resolveJob } from '../api';
  import type { JobChoice, JobUpdate, ReleaseCandidate } from '../types';
  let { job }: { job: JobUpdate & ({ state: 'needs_choice' } | { state: 'duplicate' }) } = $props();

  // App.svelte wraps this component in {#key job_id}, so initial values are per job.
  // svelte-ignore state_referenced_locally
  let query = $state(job.label);
  let expanded = $state<string | null>(null);
  let busy = $state(false);

  // New candidates (after a Search) arrive as a new `job` object for the same id: re-enable buttons.
  $effect(() => {
    void job;
    busy = false;
  });

  async function choose(choice: JobChoice) {
    busy = true;
    try {
      await resolveJob(job.job_id, choice);
    } catch (e) {
      console.error('resolve_job failed', e);
      busy = false;
    }
  }

  const totalLength = (r: ReleaseCandidate) => formatDuration(r.tracks.reduce((s, t) => s + (t.length_ms ?? 0), 0));
</script>

<div class="backdrop">
  <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="picker-title">
    {#if job.state === 'duplicate'}
      <h3 id="picker-title">Already in your library</h3>
      <p><strong>{job.release.artist} – {job.release.title}</strong> ({job.release.date ?? 'unknown year'}) is already in your library.</p>
      <div class="footer">
        <button onclick={() => choose({ kind: 'skip' })} disabled={busy}>Skip</button>
        <button class="primary" onclick={() => choose({ kind: 'replace' })} disabled={busy}>Replace existing</button>
      </div>
    {:else}
      <h3 id="picker-title">Which release is “{job.label}”?</h3>
      <form class="search" onsubmit={(e) => { e.preventDefault(); if (query.trim()) choose({ kind: 'search', query: query.trim() }); }}>
        <input bind:value={query} placeholder="Search MusicBrainz (artist album)" />
        <button type="submit" disabled={busy}>Search</button>
      </form>
      <ul>
        {#each job.candidates as c (c.mbid)}
          <li>
            <img src={`https://coverartarchive.org/release/${c.mbid}/front-250`} alt="" onerror={(e) => ((e.currentTarget as HTMLImageElement).style.visibility = 'hidden')} />
            <div class="info">
              <strong>{c.title}</strong>
              <span>{c.artist}</span>
              <span class="muted">{[c.date, c.country, c.format, `${c.tracks.length} tracks`, totalLength(c)].filter(Boolean).join(' · ')}</span>
              <button class="link" onclick={() => (expanded = expanded === c.mbid ? null : c.mbid)}>{expanded === c.mbid ? 'Hide tracks' : 'Show tracks'}</button>
              {#if expanded === c.mbid}
                <ol>{#each c.tracks as t}<li>{t.title} <span class="muted">{t.length_ms ? formatDuration(t.length_ms) : ''}</span></li>{/each}</ol>
              {/if}
            </div>
            <button class="primary" onclick={() => choose({ kind: 'pick', mbid: c.mbid })} disabled={busy}>Use this</button>
          </li>
        {:else}
          <li class="muted">No releases found. Try a different search.</li>
        {/each}
      </ul>
      <div class="footer">
        <button onclick={() => choose({ kind: 'skip' })} disabled={busy}>Don't import</button>
        <button onclick={() => choose({ kind: 'keep_existing' })} disabled={busy}>Keep existing tags</button>
      </div>
    {/if}
  </div>
</div>

<style>
  .backdrop { position: fixed; inset: 0; background: rgb(0 0 0 / 0.45); display: grid; place-items: center; z-index: 60; }
  .dialog { background: var(--panel); border-radius: 12px; width: min(720px, 92vw); max-height: 86vh; overflow: auto; padding: 20px 24px; }
  h3 { margin: 0 0 12px; }
  .search { display: flex; gap: 8px; margin-bottom: 12px; }
  ul { list-style: none; padding: 0; margin: 0; }
  ul > li { display: flex; gap: 12px; align-items: flex-start; padding: 10px 0; border-bottom: 1px solid var(--border); }
  img { width: 72px; height: 72px; border-radius: 6px; object-fit: cover; background: var(--bg); flex: none; }
  .info { flex: 1; display: flex; flex-direction: column; gap: 2px; min-width: 0; }
  .muted { color: var(--muted); font-size: 12px; }
  .link { border: none; background: none; color: var(--accent); padding: 0; text-align: left; font-size: 12px; }
  ol { margin: 6px 0 0; padding-left: 20px; font-size: 12px; }
  .footer { display: flex; justify-content: flex-end; gap: 8px; margin-top: 16px; }
</style>
