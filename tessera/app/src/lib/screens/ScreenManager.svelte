<!-- Screen Management: outputs (physical displays or windows), screens (logical canvases), the
     mappings between them and the screen layout. Edits go to a local copy; Apply sends the ops
     for what changed, OK applies and closes, Cancel discards. Opened by `ui.modal = 'screens'`,
     which also keeps the show hotkeys off. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import * as api from '$lib/api';
  import type { Output } from '$lib/bindings/Output';
  import type { Screen } from '$lib/bindings/Screen';
  import type { Show } from '$lib/bindings/Show';
  import { newScreen, onAir, setupOps, type Setup } from '$lib/logic';
  import { applyOps, confirmAsk, fail, ui } from '$lib/store.svelte';
  import AddOutput from './AddOutput.svelte';
  import LayoutEditor from './LayoutEditor.svelte';
  import MappingEditor from './MappingEditor.svelte';

  let { onclose }: { onclose: () => void } = $props();

  const copy = (s: Show | null): Setup => structuredClone({ outputs: s?.outputs ?? [], screens: s?.screens ?? [], mappings: s?.mappings ?? [] });
  const initial = copy(ui.show);
  let buf = $state(initial);
  let outputId = $state(initial.outputs[0]?.id ?? null);
  let screenId = $state(initial.screens[0]?.id ?? null);
  let mappingId = $state<string | null>(null);
  let adding = $state(false);
  let busy = $state(false);
  let error = $state('');

  onMount(() => void api.listMonitors().catch(fail));

  /** What Apply would send now. */
  const ops = $derived(ui.show ? setupOps(ui.show, buf) : []);
  const n = (k: number, what: string) => `${k} ${what}${k === 1 ? '' : 's'}`;

  function status(id: string): string {
    if (!ui.show?.outputs.some((o) => o.id === id)) return 'not applied';
    const st = ui.outputs.find((s) => s.outputId === id);
    return st ? (st.bound ? 'Bound' : 'Unbound') : '…';
  }

  function addOutput(o: Output) {
    buf.outputs.push(o);
    outputId = o.id;
    adding = false;
  }

  async function removeOutput(o: Output) {
    const maps = buf.mappings.filter((m) => m.outputId === o.id).length;
    if (!(await confirmAsk(`Remove output “${o.name}”? On Apply this also removes its ${n(maps, 'mapping')}.`, 'Remove'))) return;
    buf.outputs = buf.outputs.filter((x) => x.id !== o.id);
    buf.mappings = buf.mappings.filter((m) => m.outputId !== o.id);
    if (outputId === o.id) outputId = buf.outputs[0]?.id ?? null;
  }

  function addScreen() {
    const s = newScreen(buf.screens);
    buf.screens.push(s);
    screenId = s.id;
  }

  async function removeScreen(s: Screen) {
    const maps = buf.mappings.filter((m) => m.screenId === s.id).length;
    const cues = ui.show?.groups.flatMap((g) => g.cues).filter((c) => s.id in c.layersByScreen).length ?? 0;
    const msg = `Remove screen “${s.name}”? On Apply this also removes its ${n(maps, 'mapping')} and its layers from every cue (used in ${n(cues, 'cue')}).`;
    if (!(await confirmAsk(msg, 'Remove'))) return;
    buf.screens = buf.screens.filter((x) => x.id !== s.id);
    buf.mappings = buf.mappings.filter((m) => m.screenId !== s.id);
    if (screenId === s.id) screenId = buf.screens[0]?.id ?? null;
  }

  /** Send the changes; true when everything was applied (or nothing needed to be). */
  async function apply(): Promise<boolean> {
    const list = ui.show ? setupOps(ui.show, $state.snapshot(buf)) : [];
    if (!list.length) return true;
    if (onAir(ui.live) && !(await confirmAsk(`This changes outputs on air: ${n(list.length, 'change')} to outputs, screens and mappings take effect on Program now.`, 'Apply on air'))) {
      return false;
    }
    busy = true;
    error = await applyOps(list);
    busy = false;
    if (!error) buf = copy(ui.show);
    return !error;
  }

  async function ok() {
    if (await apply()) onclose();
  }

  async function cancel() {
    if (ops.length && !(await confirmAsk(`Discard ${n(ops.length, 'change')} not applied yet?`, 'Discard'))) return;
    onclose();
  }
</script>

{#snippet eye()}<svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true"
    ><path d="M1 8s2.6-5 7-5 7 5 7 5-2.6 5-7 5-7-5-7-5z" fill="none" stroke="currentColor" stroke-width="1.4" /><circle cx="8" cy="8" r="2.2" fill="currentColor" /></svg
  >{/snippet}
{#snippet trash()}<svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true"
    ><path d="M2.5 4h11M6 4V2.5h4V4M4 4l.8 9.5h6.4L12 4" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round" /></svg
  >{/snippet}

<div class="backdrop">
  <div class="sm">
    <header><h3>Screen Management</h3></header>

    <aside class="side">
      <section class="panel list">
        <h2>Outputs <button class="mini" onclick={() => (adding = true)}>+ Add</button></h2>
        <ul>
          <li class="node">▾ Local</li>
          {#each buf.outputs as o (o.id)}
            {@const st = status(o.id)}
            <li class="item child" class:sel={o.id === outputId}>
              <button class="pick" onclick={() => (outputId = o.id)} title={ui.outputs.find((s) => s.outputId === o.id)?.monitor ?? ''}>
                <span class="name"><i class="dot" class:ok={st === 'Bound'} class:bad={st === 'Unbound'}></i>{o.name}</span>
                <span class="muted">{o.target.type === 'Window' ? 'window' : 'display'} · {o.width}×{o.height} · {st}</span>
              </button>
              <button class="icon" title={st === 'not applied' ? 'Apply first to identify it' : 'Identify'} disabled={st === 'not applied'} onclick={() => api.identify(o.id).catch(fail)}
                >{@render eye()}</button
              >
              <button class="icon" title="Remove output" onclick={() => removeOutput(o)}>{@render trash()}</button>
            </li>
          {:else}
            <li class="muted empty">No outputs yet.</li>
          {/each}
        </ul>
      </section>
      <section class="panel list">
        <h2>Screens <button class="mini" onclick={addScreen}>+ Add</button></h2>
        <ul>
          {#each buf.screens as s (s.id)}
            <li class="item" class:sel={s.id === screenId}>
              <button class="pick" onclick={() => (screenId = s.id)}>
                <span class="name">{s.name}</span>
                <span class="muted">{s.width}×{s.height} · {n(buf.mappings.filter((m) => m.screenId === s.id).length, 'mapping')}</span>
              </button>
              <button class="icon" title="Remove screen" onclick={() => removeScreen(s)}>{@render trash()}</button>
            </li>
          {:else}
            <li class="muted empty">No screens yet.</li>
          {/each}
        </ul>
      </section>
    </aside>

    <div class="editors">
      <MappingEditor bind:buf {outputId} bind:mappingId />
      <LayoutEditor bind:buf bind:screenId />
    </div>

    <footer>
      {#if error}
        <button class="error" title="Dismiss" onclick={() => (error = '')}>{error}</button>
      {:else}
        <span class="muted">{ops.length ? `${n(ops.length, 'change')} not applied` : 'No changes'}</span>
      {/if}
      <span class="actions">
        <button onclick={cancel} disabled={busy}>Cancel</button>
        <button onclick={apply} disabled={busy || !ops.length}>Apply</button>
        <button class="primary" onclick={ok} disabled={busy}>OK</button>
      </span>
    </footer>
  </div>
  {#if adding}
    <AddOutput count={buf.outputs.length} onadd={addOutput} onclose={() => (adding = false)} />
  {/if}
</div>

<style>
  .sm {
    width: min(1280px, 96vw);
    height: min(860px, 94vh);
    display: grid;
    grid-template-columns: 280px minmax(0, 1fr);
    grid-template-rows: auto minmax(0, 1fr) auto;
    grid-template-areas: 'head head' 'side editors' 'foot foot';
    gap: 6px;
    padding: 10px 12px;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 6px;
    box-shadow: 0 12px 40px rgba(0, 0, 0, 0.6);
  }
  header {
    grid-area: head;
  }
  h3 {
    margin: 0;
    font-size: 13px;
  }
  .side {
    grid-area: side;
    display: grid;
    grid-template-rows: minmax(0, 1fr) minmax(0, 1fr);
    gap: 6px;
    min-height: 0;
  }
  .editors {
    grid-area: editors;
    display: grid;
    grid-template-rows: minmax(0, 1.25fr) minmax(0, 1fr);
    gap: 6px;
    min-height: 0;
  }
  .mini {
    margin-left: auto;
    padding: 1px 7px;
    text-transform: none;
    letter-spacing: 0;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 4px 0;
    overflow: auto;
  }
  .node {
    padding: 2px 8px;
    color: var(--muted);
    font-weight: 600;
  }
  .item {
    display: flex;
    align-items: center;
    gap: 2px;
    padding: 0 4px;
  }
  .item.child {
    padding-left: 16px;
  }
  .item.sel {
    background: var(--accent-bg);
    box-shadow: inset 3px 0 var(--accent);
  }
  .pick {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    text-align: left;
    background: transparent;
    border: 0;
    padding: 4px;
  }
  .pick > span {
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .pick .name {
    font-weight: 500;
  }
  .pick .muted {
    font-size: 11px;
  }
  .dot {
    display: inline-block;
    width: 7px;
    height: 7px;
    margin-right: 5px;
    border-radius: 50%;
    background: var(--muted);
    vertical-align: 1px;
  }
  .dot.ok {
    background: var(--pvw);
  }
  .dot.bad {
    background: var(--warn);
  }
  .icon {
    background: transparent;
    border-color: transparent;
    padding: 3px 5px;
    color: var(--muted);
    display: grid;
  }
  .icon:hover:not(:disabled) {
    color: var(--text);
  }
  .empty {
    padding: 4px 10px;
  }
  footer {
    grid-area: foot;
    display: flex;
    align-items: center;
    gap: 8px;
    padding-top: 4px;
  }
  .actions {
    margin-left: auto;
    display: flex;
    gap: 6px;
  }
  .actions button {
    min-width: 72px;
  }
  .error {
    background: transparent;
    border-color: var(--live);
    color: var(--live);
    white-space: normal;
    text-align: left;
  }
</style>
