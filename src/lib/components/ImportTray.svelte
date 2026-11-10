<script lang="ts">
  import type { JobUpdate } from '../types';
  let { jobs, onClear }: { jobs: JobUpdate[]; onClear: () => void } = $props();

  const text: Record<JobUpdate['state'], string> = {
    queued: 'Waiting',
    identifying: 'Looking up…',
    needs_choice: 'Needs your pick',
    duplicate: 'Already in library',
    importing: 'Copying & tagging…',
    done: 'Added',
    skipped: 'Skipped',
    failed: 'Failed',
  };
  function detail(j: JobUpdate): string {
    if (j.state === 'done' && !j.matched) return 'Added — no match found';
    if (j.state === 'failed') return `Failed: ${j.error}`;
    return text[j.state];
  }
  const finished = (j: JobUpdate) => j.state === 'done' || j.state === 'skipped' || j.state === 'failed';
</script>

{#if jobs.length > 0}
  <aside>
    <div class="head">
      <strong>Imports</strong>
      <button onclick={onClear} disabled={!jobs.some(finished)}>Clear finished</button>
    </div>
    <ul>
      {#each jobs as j (j.job_id)}
        <li class={j.state}>
          <span class="label">{j.label}</span>
          <span class="state">{detail(j)}</span>
        </li>
      {/each}
    </ul>
  </aside>
{/if}

<style>
  aside { position: fixed; right: 16px; bottom: 16px; width: 340px; max-height: 40vh; overflow: auto; background: var(--panel); border: 1px solid var(--border); border-radius: var(--radius); box-shadow: 0 4px 16px rgb(0 0 0 / 0.2); z-index: 40; }
  .head { display: flex; justify-content: space-between; align-items: center; padding: 8px 12px; border-bottom: 1px solid var(--border); }
  .head button { padding: 2px 8px; font-size: 12px; }
  ul { list-style: none; margin: 0; padding: 0; }
  li { padding: 8px 12px; border-bottom: 1px solid var(--border); display: flex; flex-direction: column; gap: 2px; }
  .label { font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .state { font-size: 12px; color: var(--muted); }
  li.failed .state { color: var(--danger); }
  li.needs_choice .state, li.duplicate .state { color: var(--warn); }
</style>
