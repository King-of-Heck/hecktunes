<script lang="ts">
  import * as api from '../api';
  import { formatDuration } from '../api';
  import type { PlayerStatus, QueueItem } from '../types';
  let {
    status,
    item,
    hasNext,
    playError = null,
    onPrevious,
    onNext,
  }: { status: PlayerStatus | null; item: QueueItem | null; hasNext: boolean; playError?: string | null; onPrevious: () => void; onNext: () => void } = $props();

  // While dragging the slider, show the dragged position instead of the live one.
  let dragging = $state<number | null>(null);
  const playing = $derived(status?.state === 'playing');
  const position = $derived(dragging ?? status?.position_ms ?? 0);
  const duration = $derived(status?.duration_ms ?? 0);

  const fail = (e: unknown) => console.error('player command failed', e);
  const toggle = () => (playing ? api.playerPause() : api.playerResume()).catch(fail);
  function seek(ms: number) {
    dragging = null;
    api.playerSeek(ms).catch(fail);
  }
</script>

{#if item}
  <footer class="bar">
    <div class="what">
      <strong>{item.title}</strong>
      <span>{item.artist} · {item.album_title}</span>
      {#if playError}<span class="err">{playError}</span>{:else if status?.state === 'error'}<span class="err">{status.error}</span>{/if}
    </div>
    <div class="controls">
      <button title="Previous" onclick={onPrevious}>⏮</button>
      <button class="main" title={playing ? 'Pause' : 'Play'} onclick={toggle} disabled={status?.state === 'error' || status?.state === 'idle'}>
        {playing ? '❚❚' : '▶'}
      </button>
      <button title="Next" onclick={onNext} disabled={!hasNext}>⏭</button>
    </div>
    <div class="seek">
      <span class="time">{formatDuration(position)}</span>
      <input
        type="range"
        min="0"
        max={Math.max(duration, 1)}
        step="1000"
        value={position}
        disabled={duration === 0}
        oninput={(e) => (dragging = Number(e.currentTarget.value))}
        onchange={(e) => seek(Number(e.currentTarget.value))}
        onpointerup={() => (dragging = null)}
        onblur={() => (dragging = null)}
        aria-label="Position"
      />
      <span class="time">{formatDuration(duration)}</span>
    </div>
    <button class="close" title="Stop" onclick={() => api.playerStop().catch(fail)}>■</button>
  </footer>
{/if}

<style>
  .bar { display: grid; grid-template-columns: minmax(180px, 1fr) auto minmax(200px, 2fr) auto; align-items: center; gap: 16px; padding: 8px 16px; background: var(--panel); border-top: 1px solid var(--border); }
  .what { display: flex; flex-direction: column; min-width: 0; }
  .what strong, .what span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .what span { color: var(--muted); font-size: 12px; }
  .what .err { color: var(--danger); }
  .controls { display: flex; gap: 6px; }
  .controls button, .close { border: none; background: transparent; font-size: 16px; padding: 4px 8px; }
  .controls .main { font-size: 18px; }
  .seek { display: flex; align-items: center; gap: 8px; }
  .seek input { flex: 1; }
  .time { color: var(--muted); font-size: 12px; font-variant-numeric: tabular-nums; min-width: 36px; text-align: center; }
</style>
