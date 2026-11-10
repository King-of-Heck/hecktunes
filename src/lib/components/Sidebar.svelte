<script lang="ts">
  import type { View } from '../types';
  let { view, needsCount, onSelect }: { view: View; needsCount: number; onSelect: (v: View) => void } = $props();
</script>

<nav>
  <h1>HeckTunes</h1>
  <p class="section">Library</p>
  <button class:active={view === 'albums'} onclick={() => onSelect('albums')}>Albums</button>
  <button class:active={view === 'needs_metadata'} onclick={() => onSelect('needs_metadata')}>
    Needs metadata {#if needsCount > 0}<span class="badge">{needsCount}</span>{/if}
  </button>
</nav>

<style>
  nav { background: var(--sidebar); border-right: 1px solid var(--border); padding: 16px 10px; display: flex; flex-direction: column; gap: 2px; }
  h1 { font-size: 18px; margin: 0 8px 16px; color: var(--accent); }
  .section { font-size: 11px; text-transform: uppercase; letter-spacing: 0.06em; color: var(--muted); margin: 8px 8px 4px; }
  button { border: none; background: transparent; text-align: left; display: flex; justify-content: space-between; align-items: center; }
  button.active { background: var(--panel); font-weight: 600; }
  .badge { background: var(--warn); color: #fff; border-radius: 10px; padding: 0 7px; font-size: 11px; }
</style>
