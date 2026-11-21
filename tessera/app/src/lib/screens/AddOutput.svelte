<!-- Add Output: a monitor (fullscreen Display output, bound by name + position) or a Window. -->
<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import { currentMonitor } from '@tauri-apps/api/window';
  import * as api from '$lib/api';
  import type { MonitorInfo } from '$lib/bindings/MonitorInfo';
  import type { Output } from '$lib/bindings/Output';
  import { displayOutput, windowOutput } from '$lib/logic';
  import { confirmAsk, fail, ui } from '$lib/store.svelte';
  import NumField from './NumField.svelte';

  let { count, onadd, onclose }: { count: number; onadd: (o: Output) => void; onclose: () => void } = $props();

  const refresh = () => api.listMonitors().catch(fail);
  onMount(refresh);

  /** Where this window is (I3): a fullscreen output there covers the controller. The main display
   *  (0,0) until the window says otherwise. */
  let here = $state({ x: 0, y: 0 });
  onMount(() => {
    currentMonitor()
      .then((m) => m && (here = { x: m.position.x, y: m.position.y }))
      .catch(fail);
  });
  const isHere = (m: MonitorInfo) => m.x === here.x && m.y === here.y;

  // Defaults from when the dialog opened.
  const next = untrack(() => count) + 1;
  const winName = `Window ${next}`;
  const start = 60 + 40 * next;
  /** null = a Window output. */
  let pick = $state.raw<MonitorInfo | null>(null);
  let name = $state(winName);
  let w = $state(960);
  let h = $state(540);
  let x = $state(start);
  let y = $state(start);

  const key = (m: MonitorInfo | null) => (m ? `${m.name}@${m.x},${m.y}` : 'window');
  /** Some output is on this monitor now (latest OutputStatus). */
  const bound = (m: MonitorInfo) => ui.outputs.some((s) => s.bound && s.monitor === m.name);

  function choose(m: MonitorInfo | null) {
    pick = m;
    name = m ? m.name : winName;
    [w, h] = m ? [m.width, m.height] : [960, 540];
  }

  async function add() {
    const n = name.trim() || (pick ? pick.name : winName);
    if (pick && isHere(pick)) {
      // The engine keeps outputs on the main display at normal window level, elsewhere above all.
      const msg = `${pick.name || 'This monitor'} is the controller's display. A fullscreen output there covers this window. ${
        pick.x === 0 && pick.y === 0
          ? '⌘-Tab or the Tessera Dock icon brings the controller back in front.'
          : 'It stays above every window, this one included: move Tessera to another display first.'
      }`;
      if (!(await confirmAsk(msg, 'Add on controller display'))) return;
    }
    onadd(pick ? { ...displayOutput(pick, n), width: w, height: h } : windowOutput(n, x, y, w, h));
  }
</script>

<div class="backdrop">
  <div class="dialog add">
    <h3>Add Output <button class="refresh" onclick={refresh}>Refresh monitors</button></h3>
    <ul>
      {#each ui.monitors as m (key(m))}
        <li>
          <button class="row" class:on={key(pick) === key(m)} onclick={() => choose(m)}>
            <span class="name">{m.name || 'Unnamed monitor'}{#if isHere(m)}<span class="here"> · Controller display</span>{/if}</span>
            <span class="muted">{m.x},{m.y}</span>
            <span class="muted">{m.width}×{m.height}</span>
            <span class="state" class:ok={bound(m)}>{bound(m) ? 'Bound' : 'Unbound'}</span>
          </button>
        </li>
      {:else}
        <li class="muted none">No monitors reported yet.</li>
      {/each}
      <li>
        <button class="row" class:on={!pick} onclick={() => choose(null)}>
          <span class="name">Window</span><span class="muted">a movable window on this machine</span>
        </button>
      </li>
    </ul>
    <div class="fields">
      <label class="text"><span>Name</span><input bind:value={name} /></label>
      <NumField label="W" value={w} int min={1} onset={(v) => (w = v)} />
      <NumField label="H" value={h} int min={1} onset={(v) => (h = v)} />
      {#if !pick}
        <NumField label="X" value={x} int onset={(v) => (x = v)} />
        <NumField label="Y" value={y} int onset={(v) => (y = v)} />
      {/if}
    </div>
    <p class="muted hint">
      {pick ? `Fullscreen on ${pick.name || 'this monitor'}, found again by its name, else its position.` : 'Position and size in physical pixels.'}
    </p>
    <div class="actions">
      <button onclick={onclose}>Cancel</button>
      <button class="primary" onclick={add}>Add</button>
    </div>
  </div>
</div>

<style>
  .add {
    width: 520px;
  }
  h3 {
    display: flex;
    align-items: center;
  }
  .refresh {
    margin-left: auto;
    font-weight: 400;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    max-height: 220px;
    overflow: auto;
  }
  .row {
    display: flex;
    gap: 12px;
    width: 100%;
    text-align: left;
    border: 0;
    border-radius: 0;
    background: transparent;
    padding: 6px 8px;
  }
  .row.on {
    background: var(--accent-bg);
    box-shadow: inset 3px 0 var(--accent);
  }
  .name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .state {
    color: var(--muted);
    width: 56px;
  }
  .here {
    color: var(--warn);
  }
  .state.ok {
    color: var(--pvw);
  }
  .none {
    padding: 6px 8px;
  }
  .fields {
    display: flex;
    gap: 8px;
    align-items: flex-end;
    margin-top: 12px;
  }
  .text {
    display: flex;
    flex-direction: column;
    gap: 1px;
    flex: 1;
  }
  .text span {
    font-size: 10px;
    color: var(--muted);
  }
  .hint {
    margin: 14px 0 0;
  }
</style>
