<!-- Layout editor: every screen at its layout position, scaled to fit. Drag to move (the buffer
     changes on release); the fields set position and the screen's size. -->
<script lang="ts">
  import type { Rect } from '$lib/bindings/Rect';
  import { dragRect, fitScale, layoutBounds, type Bounds, type Setup } from '$lib/logic';
  import NumField from './NumField.svelte';

  let { buf = $bindable(), screenId = $bindable() }: { buf: Setup; screenId: string | null } = $props();

  let cw = $state(0);
  let ch = $state(0);

  type Drag = { id: string; x0: number; y0: number; start: Rect; rect: Rect; bounds: Bounds; scale: number };
  let drag = $state.raw<Drag | null>(null);

  // Frozen while dragging, so the view doesn't rescale under the pointer.
  const bounds = $derived(drag?.bounds ?? layoutBounds(buf.screens));
  const scale = $derived(drag?.scale ?? fitScale(bounds, cw - 24, ch - 24));
  const sel = $derived(buf.screens.find((s) => s.id === screenId));
  const hue = (i: number) => (28 + i * 137) % 360;

  function down(e: PointerEvent, id: string, start: Rect) {
    if (e.button !== 0) return;
    e.stopPropagation();
    screenId = id;
    (e.currentTarget as Element).setPointerCapture(e.pointerId);
    drag = { id, x0: e.clientX, y0: e.clientY, start, rect: start, bounds, scale };
  }
  function move(e: PointerEvent) {
    if (drag && drag.scale > 0) drag = { ...drag, rect: dragRect(drag.start, (e.clientX - drag.x0) / drag.scale, (e.clientY - drag.y0) / drag.scale, 'move') };
  }
  function up() {
    const s = drag && buf.screens.find((x) => x.id === drag!.id);
    if (s && drag) [s.layoutX, s.layoutY] = [drag.rect.x, drag.rect.y];
    drag = null;
  }
</script>

<section class="panel editor">
  <h2>Layout <span class="sub">where screens sit in the preview</span></h2>
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="area" bind:clientWidth={cw} bind:clientHeight={ch} onpointerdown={() => (screenId = null)}>
    {#if buf.screens.length}
      <div
        class="stage"
        style:width="{bounds.w * scale}px"
        style:height="{bounds.h * scale}px"
        onpointermove={move}
        onpointerup={up}
        onpointercancel={() => (drag = null)}
      >
        {#each buf.screens as s, i (s.id)}
          {@const r = drag?.id === s.id ? drag.rect : { x: s.layoutX, y: s.layoutY, w: s.width, h: s.height }}
          <div
            class="screen"
            class:sel={s.id === screenId}
            style:--hue={hue(i)}
            style:left="{(r.x - bounds.x) * scale}px"
            style:top="{(r.y - bounds.y) * scale}px"
            style:width="{r.w * scale}px"
            style:height="{r.h * scale}px"
            onpointerdown={(e) => down(e, s.id, r)}
          >
            <span class="tag">{s.name} <small>{s.width}×{s.height} at {r.x},{r.y}</small></span>
          </div>
        {/each}
      </div>
    {:else}
      <p class="muted">Add a screen: a canvas cues draw on, e.g. a 3840×1080 LED wall.</p>
    {/if}
  </div>
  <div class="bar">
    {#if sel}
      {#key sel.id}
        <label class="text"><span>Screen name</span><input value={sel.name} onchange={(e) => (e.currentTarget.value = sel.name = e.currentTarget.value.trim() || sel.name)} /></label>
        <NumField label="X" value={sel.layoutX} int onset={(v) => (sel.layoutX = v)} />
        <NumField label="Y" value={sel.layoutY} int onset={(v) => (sel.layoutY = v)} />
        <NumField label="W" value={sel.width} int min={1} onset={(v) => (sel.width = v)} />
        <NumField label="H" value={sel.height} int min={1} onset={(v) => (sel.height = v)} />
      {/key}
    {:else}
      <span class="muted">{buf.screens.length ? 'Select a screen to place or resize it.' : ''}</span>
    {/if}
  </div>
</section>

<style>
  .editor {
    min-height: 0;
  }
  .sub {
    text-transform: none;
    letter-spacing: 0;
    font-weight: 400;
  }
  .area {
    flex: 1;
    min-height: 0;
    display: grid;
    place-items: center;
    background: #08090b;
    overflow: hidden;
  }
  .stage {
    position: relative;
    touch-action: none;
  }
  .screen {
    position: absolute;
    background: hsl(var(--hue) 80% 55% / 0.16);
    outline: 1px solid hsl(var(--hue) 80% 60% / 0.8);
    cursor: move;
  }
  .screen.sel {
    outline: 2px solid var(--accent);
    background: hsl(var(--hue) 80% 55% / 0.3);
    z-index: 1;
  }
  .tag {
    position: absolute;
    left: 0;
    top: 0;
    max-width: 100%;
    padding: 0 4px;
    font-size: 10px;
    background: rgba(0, 0, 0, 0.6);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    pointer-events: none;
  }
  .tag small {
    color: var(--muted);
  }
  .bar {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 8px;
    align-items: flex-end;
    padding: 6px 8px;
    min-height: 44px;
  }
  .text {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .text span {
    font-size: 10px;
    color: var(--muted);
  }
</style>
