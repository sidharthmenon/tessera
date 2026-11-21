<script lang="ts">
  import type { Layer } from '$lib/bindings/Layer';
  import type { Rect } from '$lib/bindings/Rect';
  import type { Screen } from '$lib/bindings/Screen';
  import { boxOf, cueAt, dragRect, engineUrl, fitScale, layoutBounds, mediaOf, screenAt, snapDrag, type DragMode } from '$lib/logic';
  import { addToPvw, editLayer, fail, ui } from '$lib/store.svelte';

  let cw = $state(0);
  let ch = $state(0);
  let stage = $state<HTMLDivElement>();

  const screens = $derived(ui.show?.screens ?? []);
  const bounds = $derived(layoutBounds(screens));
  const scale = $derived(fitScale(bounds, cw - 16, ch - 16));
  const url = $derived(engineUrl(ui.engine, `/preview/${ui.canvasTab}.mjpg`));
  const pgmCue = $derived(cueAt(ui.show, ui.live.lastTaken));

  type Drag = { screenId: string; layerId: string; mode: DragMode; x0: number; y0: number; start: Rect; rect: Rect };
  let drag = $state.raw<Drag | null>(null);

  // Everything the bus shows, all of it editable.
  const boxes = $derived(
    screens.flatMap((s) =>
      (ui.busScene[s.id] ?? []).map((l) => {
        const rect = drag?.layerId === l.id ? drag.rect : l.rect;
        return { s, l, box: boxOf(rect, s, bounds, scale) };
      }),
    ),
  );

  const label = (l: Layer) => l.name || (l.source.type === 'Media' ? (mediaOf(ui.show, l)?.name ?? 'media') : l.source.pattern.type);

  function down(e: PointerEvent, s: Screen, l: Layer, mode: DragMode) {
    if (e.button !== 0) return;
    e.stopPropagation();
    ui.selected = { screenId: s.id, layerId: l.id };
    (e.currentTarget as Element).setPointerCapture(e.pointerId);
    drag = { screenId: s.id, layerId: l.id, mode, x0: e.clientX, y0: e.clientY, start: l.rect, rect: l.rect };
  }

  /** Edges snap to the layer's screen and to the other layers on it. A resize keeps the layer's
   *  shape while Keep aspect is on; Shift does the other. */
  function move(e: PointerEvent) {
    const s = drag && screens.find((x) => x.id === drag!.screenId);
    if (!drag || !s || scale <= 0) return;
    const r = dragRect(drag.start, (e.clientX - drag.x0) / scale, (e.clientY - drag.y0) / scale, drag.mode);
    const others = (ui.busScene[s.id] ?? []).filter((l) => l.id !== drag!.layerId).map((l) => l.rect);
    const aspect = ui.keepAspect !== e.shiftKey ? drag.start.w / drag.start.h : undefined;
    drag = { ...drag, rect: snapDrag(r, drag.mode, others, s.width, s.height, scale, aspect) };
  }

  /** One edit per drag, on release. */
  async function up() {
    const d = drag;
    if (!d) return;
    const r = d.rect;
    if (r.x !== d.start.x || r.y !== d.start.y || r.w !== d.start.w || r.h !== d.start.h) {
      await editLayer({ screenId: d.screenId, layerId: d.layerId }, (l) => ({ ...l, rect: r }));
    }
    if (drag === d) drag = null; // after the edit lands, so the box doesn't jump back meanwhile
  }

  /** The screen under the pointer. */
  function screenUnder(e: MouseEvent) {
    if (!stage || scale <= 0) return undefined;
    const r = stage.getBoundingClientRect();
    return screenAt(screens, bounds.x + (e.clientX - r.left) / scale, bounds.y + (e.clientY - r.top) / scale);
  }

  /** The editable layer box under a drag. */
  function boxUnder(e: DragEvent) {
    const id = (e.target as Element).closest?.('[data-layer]')?.getAttribute('data-layer');
    return boxes.find((b) => b.l.id === id);
  }

  /** Where a drag from Resources would drop: onto a layer, else onto a screen. */
  let hover = $state<{ screenId?: string; layerId?: string }>({});

  /** Dropped onto a layer, media replaces its source (rect and settings kept); elsewhere it fills
   *  the screen it lands on as a new layer. */
  function drop(e: DragEvent) {
    e.preventDefault();
    const item = ui.dragging;
    ui.dragging = null;
    hover = {};
    if (!item) return;
    const b = boxUnder(e);
    if (b) {
      ui.selected = { screenId: b.s.id, layerId: b.l.id };
      return editLayer(ui.selected, (l) => ({ ...l, source: item.source, name: item.name }));
    }
    const screen = screenUnder(e);
    if (!screen) return fail('Drop onto a screen');
    addToPvw(item.source, item.name, screen);
  }
</script>

<section class="panel canvas">
  <div class="panel-head">
    <div class="tabs">
      <button class:on={ui.canvasTab === 'pgm'} onclick={() => (ui.canvasTab = 'pgm')}>Program</button>
      <button class:on={ui.canvasTab === 'pvw'} onclick={() => (ui.canvasTab = 'pvw')}>Preview</button>
    </div>
    {#if ui.canvasTab === 'pgm'}
      <!-- M2: an adopted engine never gets SetProgram, so its live edits wait for the next GO. -->
      <span class="badge live">ON AIR</span><span class="note warn">{ui.engine?.adopted ? 'edits reach air on the next GO' : 'edits go to air now'}</span>
    {/if}
    <label class="aspect" title="Resizing keeps a layer's shape. Hold Shift while dragging for the other."><input type="checkbox" bind:checked={ui.keepAspect} /> Keep aspect</label>
    {#if ui.canvasTab === 'pgm'}
      <span class="cue">{pgmCue ? `${pgmCue.slot + 1} · ${pgmCue.name}` : Object.keys(ui.live.program).length ? 'live' : 'nothing taken yet'}</span>
    {:else if ui.pvwCue}
      <span class="cue">
        {ui.pvwCue.slot + 1} · {ui.pvwCue.name}
        {#if ui.pvwEdited}<span class="edited" title="Preview differs from this cue: right-click a cue → Update with current view, or click an empty slot to store it">· edited</span>{/if}
      </span>
    {:else}
      <span class="cue muted">{Object.keys(ui.live.preview).length ? 'not stored in a cue: click an empty slot to store it' : 'nothing in preview'}</span>
    {/if}
  </div>
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="area"
    class:live={ui.canvasTab === 'pgm'}
    bind:clientWidth={cw}
    bind:clientHeight={ch}
    ondragover={(e) => {
      if (!ui.dragging) return;
      e.preventDefault();
      const b = boxUnder(e);
      hover = b ? { layerId: b.l.id } : { screenId: screenUnder(e)?.id };
    }}
    ondragleave={(e) => {
      if (!(e.currentTarget as Element).contains(e.relatedTarget as Node | null)) hover = {};
    }}
    ondrop={drop}
    onpointerdown={(e) => {
      ui.selected = null;
      ui.screenId = screenUnder(e)?.id ?? null;
    }}
  >
    {#if screens.length}
      <div
        class="stage"
        bind:this={stage}
        style:width="{bounds.w * scale}px"
        style:height="{bounds.h * scale}px"
        onpointermove={move}
        onpointerup={up}
        onpointercancel={() => (drag = null)}
      >
        {#if url}
          {#key url}<img src={url} alt="" draggable="false" />{/key}
        {:else}
          <div class="offline">engine offline</div>
        {/if}
        {#each screens as s (s.id)}
          {@const b = boxOf({ x: 0, y: 0, w: s.width, h: s.height }, s, bounds, scale)}
          <div class="screen" class:target={screens.length > 1 && ui.targetScreen?.id === s.id} class:hover={!!ui.dragging && hover.screenId === s.id} style:left="{b.x}px" style:top="{b.y}px" style:width="{b.w}px" style:height="{b.h}px">
            <span>{s.name}</span>
          </div>
        {/each}
        {#each boxes as { s, l, box } (l.id)}
          <div
            class="box"
            class:sel={ui.selected?.layerId === l.id}
            class:hidden={!l.visible}
            class:drop={!!ui.dragging && hover.layerId === l.id}
            data-layer={l.id}
            style:left="{box.x}px"
            style:top="{box.y}px"
            style:width="{box.w}px"
            style:height="{box.h}px"
            onpointerdown={(e) => down(e, s, l, 'move')}
          >
            <span class="tag">{label(l)}</span>
            <div class="handle" onpointerdown={(e) => down(e, s, l, 'resize')}></div>
          </div>
        {/each}
      </div>
    {:else}
      <p class="muted">No screens yet. Set them up in Screen Management.</p>
    {/if}
  </div>
</section>

<style>
  .canvas {
    grid-area: canvas;
  }
  .note {
    text-transform: none;
    letter-spacing: 0;
    font-weight: 400;
  }
  .note.warn {
    color: var(--live);
    font-weight: 600;
  }
  .aspect {
    margin-left: auto;
    display: flex;
    align-items: center;
    gap: 4px;
    text-transform: none;
    letter-spacing: 0;
    font-weight: 400;
  }
  .edited {
    color: var(--accent);
  }
  .cue {
    text-transform: none;
    letter-spacing: 0;
    color: var(--text);
    font-weight: 500;
  }
  .area {
    flex: 1;
    min-height: 0;
    display: grid;
    place-items: center;
    background: #08090b;
    border: 2px solid transparent;
    overflow: hidden;
  }
  .area.live {
    border-color: var(--live);
  }
  .stage {
    position: relative;
    background: #000;
    touch-action: none;
  }
  .stage img {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    pointer-events: none;
  }
  .offline {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
    color: var(--muted);
  }
  .screen {
    position: absolute;
    outline: 1px dashed rgba(255, 255, 255, 0.25);
    pointer-events: none;
  }
  .screen.target {
    outline: 1px solid rgba(255, 138, 31, 0.6);
  }
  .screen.hover {
    outline: 2px solid var(--accent);
    background: var(--accent-bg);
  }
  .screen span {
    position: absolute;
    left: 3px;
    bottom: 2px;
    font-size: 10px;
    color: rgba(255, 255, 255, 0.5);
  }
  .box {
    position: absolute;
    outline: 1px solid rgba(255, 138, 31, 0.55);
    cursor: move;
  }
  .box.hidden {
    outline-style: dotted;
    opacity: 0.6;
  }
  .box.sel {
    outline: 2px solid var(--accent);
    background: var(--accent-bg);
    z-index: 1;
  }
  .box.drop {
    outline: 2px solid var(--accent);
    background: var(--accent-bg);
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
    color: var(--accent-2);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    pointer-events: none;
  }
  .handle {
    position: absolute;
    right: -5px;
    bottom: -5px;
    width: 10px;
    height: 10px;
    background: var(--accent);
    border: 1px solid #000;
    cursor: nwse-resize;
    display: none;
  }
  .sel .handle {
    display: block;
  }
</style>
