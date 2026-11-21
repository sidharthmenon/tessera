<script lang="ts">
  import '../app.css';
  import { onMount } from 'svelte';
  import { cueKey, hotkey, layerKey, ownsKeys, type Hotkey } from '$lib/logic';
  import { clickSlot, editLayer, init, loadNext, moveLayer, redo, saveShow, take, ui, undo } from '$lib/store.svelte';
  import Prompt from '$lib/Prompt.svelte';
  import Canvas from '$lib/panels/Canvas.svelte';
  import CueGrid from '$lib/panels/CueGrid.svelte';
  import OutputMonitor from '$lib/panels/OutputMonitor.svelte';
  import Properties from '$lib/panels/Properties.svelte';
  import Resources from '$lib/panels/Resources.svelte';
  import StatusBar from '$lib/panels/StatusBar.svelte';
  import Toolbar from '$lib/panels/Toolbar.svelte';
  import Transport from '$lib/panels/Transport.svelte';
  import ScreenManager from '$lib/screens/ScreenManager.svelte';

  onMount(() => {
    const ready = init();
    return () => void ready.then((off) => off());
  });

  const actions: Record<Hotkey, () => unknown> = {
    take,
    next: loadNext,
    undo,
    redo,
    save: () => saveShow(),
    saveAs: () => saveShow(true),
  };

  function onkeydown(e: KeyboardEvent) {
    const focus = document.activeElement as HTMLElement | null;
    if (ownsKeys(focus)) {
      if (e.key === 'Escape') focus?.blur(); // back to the show keys
      return; // R40: never GO from a focused control
    }
    if (ui.modal) return;
    if (layerKeys(e)) return;
    if (e.key === 'Escape' && ui.selected) return void (ui.selected = null); // → is "next cue" again
    const key = hotkey(e);
    if (!key) return cueShortcut(e);
    e.preventDefault(); // also keeps Space from clicking a focused button
    if (e.repeat && key !== 'undo' && key !== 'redo') return; // a held Space is one GO
    actions[key]();
  }

  /** With a layer selected on the Preview tab: arrows nudge it, brackets restack it. True if the
   *  key was one of those (it then does nothing else, so → doesn't also load the next cue). */
  function layerKeys(e: KeyboardEvent): boolean {
    const sel = ui.selected;
    const k = ui.canvasTab === 'pvw' && sel && ui.layer ? layerKey(e) : null;
    if (!k || !sel) return false;
    e.preventDefault();
    if ('nudge' in k) {
      const [dx, dy] = k.nudge;
      editLayer(sel, (l) => ({ ...l, rect: { ...l.rect, x: l.rect.x + dx, y: l.rect.y + dy } })); // held: repeats
    } else if (!e.repeat) {
      moveLayer(sel, k.order);
    }
    return true;
  }

  /** A cue's shortcut (active group) loads it into Preview, as a click on its tile does. */
  function cueShortcut(e: KeyboardEvent) {
    if (e.metaKey || e.ctrlKey || e.altKey || e.shiftKey || e.repeat) return;
    const k = cueKey(e);
    const group = ui.group;
    const cue = k && group?.cues.find((c) => c.hotkey === k);
    if (!cue || !group) return;
    e.preventDefault();
    clickSlot({ groupId: group.id, slot: cue.slot });
  }

  // R40: once a control has been used, hand the keys back to the show, so Space is GO again.
  // Clicked buttons and toggles lose focus; any control loses it when its value is committed
  // (`change`: a released slider, a picked option, Enter in a field). Typing keeps focus.
  const toggles = ['checkbox', 'radio', 'range', 'color', 'button', 'submit', 'reset'];
  function onclick() {
    const el = document.activeElement as HTMLInputElement | null;
    if (el?.tagName === 'BUTTON' || (el?.tagName === 'INPUT' && toggles.includes(el.type))) el.blur();
  }
  function onchange(e: Event) {
    if (e.target === document.activeElement) (e.target as HTMLElement).blur();
  }
</script>

<svelte:window
  {onkeydown}
  {onclick}
  {onchange}
  ondragover={(e) => e.preventDefault()}
  ondrop={(e) => e.preventDefault()}
/>

<div class="app">
  <Toolbar />
  <Resources />
  <Canvas />
  <aside class="side">
    <OutputMonitor />
    <Properties />
  </aside>
  <Transport />
  <CueGrid />
  <StatusBar />
</div>

{#if ui.modal === 'screens'}
  <ScreenManager onclose={() => (ui.modal = null)} />
{:else if ui.modal === 'prompt' && ui.prompt}
  <Prompt />
{/if}

<style>
  .app {
    display: grid;
    height: 100vh;
    grid-template-columns: 250px minmax(0, 1fr) 300px;
    grid-template-rows: auto minmax(0, 1fr) auto minmax(150px, 30vh) auto;
    grid-template-areas:
      'tool tool tool'
      'res canvas side'
      'transport transport transport'
      'cues cues cues'
      'status status status';
    gap: 0;
  }
  .app > :global(.panel) {
    margin: 4px;
  }
  .side {
    grid-area: side;
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 4px;
    min-height: 0;
  }
</style>
