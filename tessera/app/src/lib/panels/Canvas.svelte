<script lang="ts">
  import type { Layer } from '$lib/bindings/Layer';
  import type { Rect } from '$lib/bindings/Rect';
  import type { Screen } from '$lib/bindings/Screen';
  import { boxOf, clipTime, cropDrag, cueAt, dragRect, fmtTime, engineUrl, fitScale, layoutBounds, mediaOf, screenAt, snapDrag, type CropSide, type DragMode } from '$lib/logic';
  import { onMount } from 'svelte';
  import type { Source } from '$lib/bindings/Source';
  import { addToPvw, editLayer, fail, ui } from '$lib/store.svelte';

  let cw = $state(0);
  let ch = $state(0);
  let stage = $state<HTMLDivElement>();

  const screens = $derived(ui.show?.screens ?? []);
  const bounds = $derived(layoutBounds(screens));
  const scale = $derived(fitScale(bounds, cw - 16, ch - 16));
  const url = $derived(engineUrl(ui.engine, `/preview/${ui.canvasTab}.mjpg`));
  const pgmCue = $derived(cueAt(ui.show, ui.live.lastTaken));

  type Crop = Layer['crop'];
  type Drag = { screenId: string; layerId: string; mode: DragMode | CropSide; x0: number; y0: number; start: Rect; rect: Rect; startCrop: Crop; crop: Crop };
  let drag = $state.raw<Drag | null>(null);
  /** Crop mode: the selected layer shows edge handles that crop it (box and picture edge together). */
  let cropMode = $state(false);
  const cropSides: CropSide[] = ['left', 'top', 'right', 'bottom'];

  // Everything the bus shows, all of it editable.
  const boxes = $derived(
    screens.flatMap((s) =>
      (ui.busScene[s.id] ?? []).map((l) => {
        const rect = drag?.layerId === l.id ? drag.rect : l.rect;
        return { s, l, box: boxOf(rect, s, bounds, scale) };
      }),
    ),
  );

  const label = (l: Layer) => l.name || (l.source.type === 'Media' ? (mediaOf(ui.mediaById, l)?.name ?? 'media') : l.source.pattern.type);

  function down(e: PointerEvent, s: Screen, l: Layer, mode: DragMode | CropSide) {
    if (e.button !== 0) return;
    e.stopPropagation();
    ui.selected = { screenId: s.id, layerId: l.id };
    (e.currentTarget as Element).setPointerCapture(e.pointerId);
    drag = { screenId: s.id, layerId: l.id, mode, x0: e.clientX, y0: e.clientY, start: l.rect, rect: l.rect, startCrop: l.crop, crop: l.crop };
  }

  /** Edges snap to the layer's screen and to the other layers on it. A resize keeps the layer's
   *  shape while Keep aspect is on; Shift does the other. */
  function move(e: PointerEvent) {
    const s = drag && screens.find((x) => x.id === drag!.screenId);
    if (!drag || !s || scale <= 0) return;
    const [dx, dy] = [(e.clientX - drag.x0) / scale, (e.clientY - drag.y0) / scale];
    if (drag.mode !== 'move' && drag.mode !== 'resize') {
      // ponytail: ignores rotation (the drag runs along the screen's axes, not the turned box's).
      const side = drag.mode;
      const { rect, crop } = cropDrag(drag.start, drag.startCrop, side, side === 'left' || side === 'right' ? dx : dy);
      drag = { ...drag, rect, crop };
      return;
    }
    const r = dragRect(drag.start, dx, dy, drag.mode);
    const others = (ui.busScene[s.id] ?? []).filter((l) => l.id !== drag!.layerId).map((l) => l.rect);
    const aspect = ui.keepAspect !== e.shiftKey ? drag.start.w / drag.start.h : undefined;
    drag = { ...drag, rect: snapDrag(r, drag.mode, others, s.width, s.height, scale, aspect) };
  }

  /** One edit per drag, on release. */
  async function up() {
    const d = drag;
    if (!d) return;
    const r = d.rect;
    if (r.x !== d.start.x || r.y !== d.start.y || r.w !== d.start.w || r.h !== d.start.h || d.crop !== d.startCrop) {
      await editLayer({ screenId: d.screenId, layerId: d.layerId }, (l) => ({ ...l, rect: r, crop: d.crop }));
    }
    if (drag === d) drag = null; // after the edit lands, so the box doesn't jump back meanwhile
  }

  /** The screen under the pointer. */
  function screenUnder(e: { clientX: number; clientY: number }) {
    if (!stage || scale <= 0) return undefined;
    const r = stage.getBoundingClientRect();
    return screenAt(screens, bounds.x + (e.clientX - r.left) / scale, bounds.y + (e.clientY - r.top) / scale);
  }

  let area = $state<HTMLDivElement>();

  /** What a Resources item dropped at window point (x, y) would land on: a layer box, else a
   *  screen; `onCanvas` false off the canvas. */
  function targetAt(x: number, y: number) {
    const el = document.elementFromPoint(x, y);
    if (!el || !area?.contains(el)) return { onCanvas: false };
    const id = el.closest('[data-layer]')?.getAttribute('data-layer');
    const box = boxes.find((b) => b.l.id === id);
    return { onCanvas: true, box, screen: box ? undefined : screenUnder({ clientX: x, clientY: y }) };
  }

  /** Where the dragged Resources item would drop, for the highlight. */
  const hover = $derived.by(() => {
    if (!ui.dragging) return {};
    const t = targetAt(ui.dragging.x, ui.dragging.y);
    return { layerId: t.box?.l.id, screenId: t.screen?.id };
  });

  /** Dropped onto a layer, media replaces its source (rect and settings kept); elsewhere it fills
   *  the screen it lands on as a new layer. Off the canvas: nothing. */
  function drop(x: number, y: number, item: { source: Source; name: string }) {
    const t = targetAt(x, y);
    if (!t.onCanvas) return;
    if (t.box) {
      ui.selected = { screenId: t.box.s.id, layerId: t.box.l.id };
      return editLayer(ui.selected, (l) => ({ ...l, source: item.source, name: item.name }));
    }
    if (!t.screen) return fail('Drop onto a screen');
    addToPvw(item.source, item.name, t.screen);
  }

  onMount(() => {
    ui.canvasDrop = drop;
    return () => (ui.canvasDrop = null);
  });
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
    <button
      class="crop-toggle"
      class:on={cropMode}
      disabled={!ui.layer}
      title="Crop the selected layer by dragging its edges (the picture stays in place)"
      onclick={() => (cropMode = !cropMode)}>Crop</button
    >
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
    bind:this={area}
    bind:clientWidth={cw}
    bind:clientHeight={ch}
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
        {:else if !ui.engine}
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
            style:transform={l.rotation ? `rotate(${l.rotation}deg)` : null}
            onpointerdown={(e) => down(e, s, l, 'move')}
          >
            <span class="tag">{label(l)}</span>
            {#if l.source.type === 'Media'}
              {@const m = mediaOf(ui.mediaById, l)}
              {@const t = clipTime(m, l, ui.positions[l.id])}
              <span class="flags">{m?.kind === 'Capture' || m?.kind === 'Ndi' ? 'LIVE' : ''}{l.muted ? '🔇 ' : ''}{t ? { Loop: '⟲', Once: '→', HoldLast: '⇥' }[l.playback] : ''}</span>
              {#if t && box.h > 28}
                <span class="time">{fmtTime(t.elapsed)}<span class="rem">−{fmtTime(t.remaining)}</span></span>
                <span class="bar"><span style:width="{t.fraction * 100}%"></span></span>
              {/if}
            {/if}
            {#if cropMode && ui.selected?.layerId === l.id}
              {#each cropSides as side (side)}
                <div class="crop {side}" title="Crop {side}" onpointerdown={(e) => down(e, s, l, side)}></div>
              {/each}
            {:else}
              <div class="handle" onpointerdown={(e) => down(e, s, l, 'resize')}></div>
            {/if}
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
  .crop-toggle {
    margin-left: auto;
    padding: 0 8px;
    font-size: 11px;
    text-transform: none;
    letter-spacing: 0;
  }
  .crop-toggle.on {
    background: var(--accent);
    color: #000;
    border-color: var(--accent);
  }
  .crop {
    position: absolute;
    background: var(--accent);
    border: 1px solid #000;
  }
  .crop.left,
  .crop.right {
    top: 50%;
    width: 6px;
    height: 22px;
    margin-top: -11px;
    cursor: ew-resize;
  }
  .crop.top,
  .crop.bottom {
    left: 50%;
    width: 22px;
    height: 6px;
    margin-left: -11px;
    cursor: ns-resize;
  }
  .crop.left {
    left: -4px;
  }
  .crop.right {
    right: -4px;
  }
  .crop.top {
    top: -4px;
  }
  .crop.bottom {
    bottom: -4px;
  }
  .aspect {
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
  .flags {
    position: absolute;
    right: 2px;
    top: 0;
    font-size: 10px;
    color: var(--accent-2);
    text-shadow: 0 0 2px #000;
    pointer-events: none;
  }
  .time {
    position: absolute;
    left: 3px;
    bottom: 5px;
    font-size: 10px;
    font-variant-numeric: tabular-nums;
    color: #fff;
    background: rgba(0, 0, 0, 0.55);
    padding: 0 3px;
    pointer-events: none;
  }
  .rem {
    margin-left: 5px;
    color: var(--accent-2);
  }
  .bar {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    height: 3px;
    background: rgba(0, 0, 0, 0.5);
    pointer-events: none;
  }
  .bar span {
    display: block;
    height: 100%;
    background: var(--accent);
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
