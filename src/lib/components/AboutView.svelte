<script lang="ts">
  import { onMount } from 'svelte';
  import { openUrl } from '@tauri-apps/plugin-opener';
  import * as api from '../api';
  import type { AboutInfo } from '../types';

  let info = $state<AboutInfo | null>(null);
  let error = $state<string | null>(null);
  onMount(() => {
    api.about().then((a) => (info = a)).catch((e) => (error = String(e)));
  });
  const open = (url: string) => openUrl(url).catch((e) => (error = String(e)));
</script>

<div class="about">
  <h2>About HeckTunes</h2>
  {#if error}<p class="err">{error}</p>{/if}
  {#if info}
    <p>Version {info.version}</p>
    <p class="meta">HeckTunes licence: {info.hecktunes_license ?? 'not chosen yet'}</p>

    <section>
      <h3>echo-sync {info.echo_sync_version}</h3>
      <p>
        HeckTunes runs echo-sync, a separate program from the EchoPod project, after each sync so the Echo Mini doesn't
        have to rescan the card. echo-sync is free software under the GNU General Public License v3.
        <button class="link" onclick={() => open(info!.echo_sync_source)}>Its source code</button>
      </p>
      <p class="meta">Each HeckTunes release page also carries a copy of echo-sync's source code.</p>
      {#if info.echo_sync_license}
        <details><summary>echo-sync licence (GPLv3)</summary><pre>{info.echo_sync_license}</pre></details>
      {:else}
        <p class="warn">The echo-sync licence file is missing from this build.</p>
      {/if}
    </section>

    <section>
      <h3>Open-source components</h3>
      {#if info.third_party_notices}
        <details><summary>Third-party notices</summary><pre>{info.third_party_notices}</pre></details>
      {:else}
        <p class="warn">The third-party notices file is missing from this build.</p>
      {/if}
    </section>
  {/if}
</div>

<style>
  .about { padding: 16px 24px; max-width: 820px; }
  h2 { margin: 0 0 8px; font-size: 24px; }
  h3 { margin: 0 0 8px; font-size: 16px; }
  section { background: var(--panel); border: 1px solid var(--border); border-radius: var(--radius); padding: 12px 16px; margin: 14px 0; }
  pre { white-space: pre-wrap; font-size: 12px; max-height: 50vh; overflow: auto; background: var(--bg); padding: 8px; border-radius: 6px; }
  .meta { color: var(--muted); }
  .link { border: none; background: none; color: var(--accent); padding: 0; }
  .warn { color: var(--warn); }
  .err { color: var(--danger); }
</style>
