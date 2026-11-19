<script lang="ts">
  let { onImportVinyl }: { onImportVinyl: () => void } = $props();
</script>

<div class="tools">
  <h2>Tools</h2>
  <div class="grid">
    <section>
      <div class="icon" aria-hidden="true">◉</div>
      <div class="body">
        <h3>Import a vinyl recording</h3>
        <p class="meta">
          Pick a WAV or MP3 of a record, either one file or one per side. HeckTunes finds the gaps between songs, looks up the
          album, and lets you check the track splits before adding it to your library.
        </p>
        <button class="primary" onclick={onImportVinyl}>Choose recording…</button>
      </div>
    </section>
  </div>
</div>

<style>
  .tools { padding: 16px 24px; }
  h2 { margin: 0 0 12px; font-size: 24px; }
  h3 { margin: 0 0 4px; font-size: 16px; }
  .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(320px, 1fr)); gap: 14px; }
  section { display: flex; gap: 14px; background: var(--panel); border: 1px solid var(--border); border-radius: var(--radius); padding: 14px 16px; }
  .icon { width: 44px; height: 44px; flex: none; display: grid; place-items: center; border-radius: 50%; background: var(--bg); color: var(--accent); font-size: 24px; }
  .body { display: flex; flex-direction: column; align-items: flex-start; gap: 8px; }
  .meta { color: var(--muted); margin: 0; }
</style>
