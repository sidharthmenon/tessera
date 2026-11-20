<script lang="ts">
  import type { Layer } from '$lib/bindings/Layer';
  import type { Rect } from '$lib/bindings/Rect';
  import type { Screen } from '$lib/bindings/Screen';
  import { boxOf, dragRect, engineUrl, findLayer, fitScale, layoutBounds, mediaOf, screenAt, type DragMode } from '$lib/logic';
  import { addToPvw, editLayer, fail, ui } from '$lib/store.svelte';

  let cw = $state(0);
  let ch = $state(0);
  let stage = $state<HTMLDivElement>();

  const screens = $derived(ui.show?.screens ?? []);
  const bounds = $derived(layoutBounds(screens));
  const scale = $derived(fitScale(bounds, cw - 16, ch - 16));
  const url = $derived(engineUrl(ui.engine, `/preview/${ui.canvasTab}.mjpg`));
  const cue = $derived(ui.editCue);

  type Drag = { screenId: string; layerId: string; mode: DragMode; x0: number; y0: number; start: Rect; rect: Rect };
  let drag = $state.raw<Drag | null>(null);

  // Everything the bus shows. On PGM, layers outside the last taken cue (left by earlier cues on
  // screens it doesn't define) can be selected for transport but not edited here.
  const boxes = $derived(
    screens.flatMap((s) =>
      (ui.busScene[s.id] ?? []).map((l) => {
        const rect = drag?.layerId === l.id ? drag.rect : l.rect;
        const editable = !!cue && !!findLayer(cue.layersByScreen, s.id, l.id);
        return { s, l, editable, box: boxOf(rect, s, bounds, scale) };
      }),
    ),
  );

  const label = (l: Layer) => l.name || (l.source.type === 'Media' ? (mediaOf(ui.show, l)?.name ?? 'media') : l.source.pattern.type);

  function down(e: PointerEvent, s: Screen, l: Layer, mode: DragMode, editable: boolean) {
    if (e.button !== 0) return;
    e.stopPropagation();
    ui.selected = { screenId: s.id, layerId: l.id };
    if (!editable) return;
    (e.currentTarget as Element).setPointerCapture(e.pointerId);
    drag = { screenId: s.id, layerId: l.id, mode, x0: e.clientX, y0: e.clientY, start: l.rect, rect: l.rect };
  }

  function move(e: PointerEvent) {
    if (drag && scale > 0) drag = { ...drag, rect: dragRect(drag.start, (e.clientX - drag.x0) / scale, (e.clientY - drag.y0) / scale, drag.mode) };
  }

  /** One SetCue per drag, on release. */
  async function up() {
    const d = drag;
    if (!d) return;
    const r = d.rect;
    if (r.x !== d.start.x || r.y !== d.start.y || r.w !== d.start.w || r.h !== d.start.h) {
      await editLayer({ screenId: d.screenId, layerId: d.layerId }, (l) => ({ ...l, rect: r }));
    }
    if (drag === d) drag = null; // after the edit lands, so the box doesn't jump back meanwhile
  }

  function drop(e: DragEvent) {
    e.preventDefault();
    const item = ui.dragging;
    ui.dragging = null;
    if (!item || !stage || scale <= 0) return;
    const r = stage.getBoundingClientRect();
    const screen = screenAt(screens, bounds.x + (e.clientX - r.left) / scale, bounds.y + (e.clientY - r.top) / scale);
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
      <span class="badge live">ON AIR</span><span class="note warn">{ui.engine?.adopted ? 'edits to the last taken cue reach air on the next GO' : 'edits to the last taken cue go to air now'}</span>
    {:else if ui.pvwLive}
      <span class="badge live">LIVE</span><span class="note">this cue is on air: an edit here goes to a copy in the first free slot</span>
    {/if}
    <span class="cue">{cue ? `${cue.slot + 1} · ${cue.name}` : ui.canvasTab === 'pvw' ? 'nothing in preview' : 'nothing taken yet'}</span>
  </div>
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="area"
    class:live={ui.canvasTab === 'pgm'}
    bind:clientWidth={cw}
    bind:clientHeight={ch}
    ondragover={(e) => {
      if (ui.dragging) e.preventDefault();
    }}
    ondrop={drop}
    onpointerdown={() => (ui.selected = null)}
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
          <div class="screen" style:left="{b.x}px" style:top="{b.y}px" style:width="{b.w}px" style:height="{b.h}px">
            <span>{s.name}</span>
          </div>
        {/each}
        {#each boxes as { s, l, editable, box } (l.id)}
          <div
            class="box"
            class:sel={ui.selected?.layerId === l.id}
            class:hidden={!l.visible}
            class:fixed={!editable}
            title={editable ? '' : 'On air from an earlier cue: select it for transport; edit it in its own cue'}
            style:left="{box.x}px"
            style:top="{box.y}px"
            style:width="{box.w}px"
            style:height="{box.h}px"
            onpointerdown={(e) => down(e, s, l, 'move', editable)}
          >
            <span class="tag">{label(l)}</span>
            {#if editable}<div class="handle" onpointerdown={(e) => down(e, s, l, 'resize', true)}></div>{/if}
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
  .cue {
    margin-left: auto;
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
  .box.fixed {
    outline-style: dashed;
    cursor: pointer;
  }
  .box.sel {
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
