<script lang="ts">
  let { visible }: { visible: boolean } = $props();
</script>

{#if visible}
  <div class="overlay"><div class="box">Drop to add to your library</div></div>
{/if}

<style>
  .overlay { position: fixed; inset: 0; background: rgb(214 51 108 / 0.15); display: grid; place-items: center; pointer-events: none; z-index: 50; }
  .box { border: 3px dashed var(--accent); border-radius: 16px; padding: 48px 64px; font-size: 20px; font-weight: 600; background: var(--panel); }
</style>
