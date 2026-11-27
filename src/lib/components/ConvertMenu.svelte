<script lang="ts">
  import type { ConvertOptions } from '../types';
  let {
    label,
    disabled = false,
    onConvert,
  }: { label: string; disabled?: boolean; onConvert: (options: ConvertOptions) => Promise<void> } = $props();

  const BITRATES = [128, 160, 192, 256, 320];
  const KEY = 'hecktunes.convertOptions';
  const DEFAULTS: ConvertOptions = { bitrate_kbps: 256, switch_to_aac: true };

  // The last choices, so the next album starts from them.
  function remembered(): ConvertOptions {
    try {
      const saved = JSON.parse(localStorage.getItem(KEY) ?? 'null');
      if (saved && BITRATES.includes(saved.bitrate_kbps) && typeof saved.switch_to_aac === 'boolean') return saved;
    } catch {
      // Unreadable: fall back to the defaults.
    }
    return { ...DEFAULTS };
  }

  let open = $state(false);
  let root = $state<HTMLDetailsElement | null>(null);
  let options = $state<ConvertOptions>(remembered());
  let pop = $state<HTMLFormElement | null>(null);
  // Opens to the right of the button, or to its left when that would run off the window.
  let alignRight = $state(false);
  $effect(() => {
    if (!open || !pop) return;
    alignRight = false;
    requestAnimationFrame(() => {
      if (pop) alignRight = pop.getBoundingClientRect().right > window.innerWidth - 8;
    });
  });
  function onWindowClick(e: MouseEvent) {
    if (open && root && !root.contains(e.target as Node)) open = false;
  }

  async function start(e: SubmitEvent) {
    e.preventDefault();
    try {
      localStorage.setItem(KEY, JSON.stringify(options));
    } catch {
      // Not remembered; the conversion still runs.
    }
    open = false;
    await onConvert({ ...options });
  }
</script>

<svelte:window onclick={onWindowClick} onkeydown={(e) => e.key === 'Escape' && (open = false)} />

{#if disabled}
  <button disabled>{label}</button>
{:else}
  <details class="menu" bind:open bind:this={root}>
    <summary>{label}</summary>
    <form class="pop" class:right={alignRight} bind:this={pop} onsubmit={start}>
      <label>
        Bitrate
        <select bind:value={options.bitrate_kbps}>
          {#each BITRATES as b}<option value={b}>{b} kbps</option>{/each}
        </select>
      </label>
      <label class="check">
        <input type="checkbox" bind:checked={options.switch_to_aac} />
        Echo Mini gets the AAC files afterwards
      </label>
      <div class="buttons">
        <button type="button" onclick={() => (open = false)}>Cancel</button>
        <button type="submit" class="primary">Convert</button>
      </div>
    </form>
  </details>
{/if}

<style>
  .menu { position: relative; display: inline-block; }
  summary { list-style: none; cursor: pointer; border: 1px solid var(--border); border-radius: 6px; padding: 3px 10px; background: var(--panel); }
  summary::-webkit-details-marker { display: none; }
  .pop { position: absolute; left: 0; top: calc(100% + 4px); z-index: 30; background: var(--panel); border: 1px solid var(--border); border-radius: var(--radius); padding: 10px; display: flex; flex-direction: column; gap: 10px; box-shadow: 0 6px 20px rgba(0, 0, 0, 0.18); }
  .pop.right { left: auto; right: 0; }
  label { display: flex; align-items: center; justify-content: space-between; gap: 10px; }
  .check { justify-content: flex-start; gap: 6px; white-space: nowrap; }
  .buttons { display: flex; justify-content: flex-end; gap: 6px; }
  .primary { background: var(--accent); color: #fff; border-color: var(--accent); }
</style>
