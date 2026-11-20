<!-- Placeholder: Task 13 replaces this with the Screen Management dialog (outputs tree, Add
     Output, mapping and layout editors). Opened by `ui.modal = 'screens'`, closed with `onclose`. -->
<script lang="ts">
  import { uuid } from '$lib/logic';
  import { op, ui } from '$lib/store.svelte';

  let { onclose }: { onclose: () => void } = $props();

  /** Dev aid until T13: one 1920×1080 screen shown scaled on a 640×360 window output. */
  async function quickSetup() {
    const output = uuid();
    const screen = uuid();
    const full = { x: 0, y: 0, w: 1920, h: 1080 };
    await op({ op: 'AddOutput', output: { id: output, name: 'Window 1', node: 'local', target: { type: 'Window', x: 80, y: 80 }, width: 640, height: 360, brightness: 1 } });
    await op({ op: 'AddScreen', screen: { id: screen, name: 'Screen 1', width: 1920, height: 1080, layoutX: 0, layoutY: 0 } });
    await op({ op: 'SetMapping', mapping: { id: uuid(), screenId: screen, src: full, outputId: output, dst: { x: 0, y: 0, w: 640, h: 360 } } });
  }
</script>

<div class="backdrop">
  <div class="dialog">
    <h3>Screen Management</h3>
    <p class="muted">The outputs, mapping and layout editors come in the next build.</p>
    <p>
      Outputs: {ui.show?.outputs.map((o) => `${o.name} (${o.width}×${o.height})`).join(', ') || 'none'}<br />
      Screens: {ui.show?.screens.map((s) => `${s.name} (${s.width}×${s.height} at ${s.layoutX},${s.layoutY})`).join(', ') || 'none'}
    </p>
    <div class="actions">
      {#if !ui.show?.screens.length}<button onclick={quickSetup}>Quick setup: 1 screen on a window output</button>{/if}
      <button class="primary" onclick={onclose}>Close</button>
    </div>
  </div>
</div>
