<script lang="ts">
  import '../app.css';
  import { onMount } from 'svelte';
  import { hotkey, ownsKeys, type Hotkey } from '$lib/logic';
  import { init, loadNext, redo, saveShow, take, ui, undo } from '$lib/store.svelte';
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
    const key = hotkey(e);
    if (!key) return;
    e.preventDefault(); // also keeps Space from clicking a focused button
    if (e.repeat && key !== 'undo' && key !== 'redo') return; // a held Space is one GO
    actions[key]();
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
