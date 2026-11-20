<script lang="ts">
  import { ui } from '$lib/store.svelte';

  const text = $derived(
    {
      connected: ui.engine?.adopted ? 'engine connected (adopted: edits to the live cue reach air on the next GO)' : 'engine connected',
      held: 'engine held: kept from an earlier session, outputs untouched until you open or create a show',
      restarting: ui.restarting || 'engine restarting',
      connecting: 'engine connecting…',
    }[ui.status],
  );
  const bound = $derived(ui.outputs.filter((o) => o.bound).length);
</script>

<footer class="status">
  <span class="engine {ui.status}"><span class="dot"></span>{text}</span>
  {#if ui.stats}
    <span>{ui.stats.fps.toFixed(1)} fps</span>
    <span class:bad={ui.stats.dropped > 0}>{ui.stats.dropped} dropped</span>
  {/if}
  {#if ui.outputs.length}<span>outputs {bound}/{ui.outputs.length} bound</span>{/if}
  {#if ui.error}
    <button class="error" title="Dismiss" onclick={() => (ui.error = '')}>⚠ {ui.error}</button>
  {/if}
</footer>

<style>
  .status {
    grid-area: status;
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 3px 10px;
    background: var(--panel);
    border-top: 1px solid var(--border);
    color: var(--muted);
    font-size: 11px;
    white-space: nowrap;
    min-width: 0;
  }
  .engine {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--muted);
  }
  .connected .dot {
    background: var(--pvw);
  }
  .held .dot {
    background: var(--warn);
  }
  .held,
  .restarting {
    color: var(--warn);
  }
  .restarting .dot {
    background: var(--live);
  }
  .bad {
    color: var(--warn);
  }
  .error {
    margin-left: auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    background: rgba(239, 59, 50, 0.18);
    border-color: var(--live);
    color: #ffb4ae;
    padding: 0 8px;
  }
</style>
