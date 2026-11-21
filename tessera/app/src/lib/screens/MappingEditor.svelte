<!-- Mapping editor: the selected output, scaled to fit, with the screen slices mapped onto it
     (dst rects). Drag a slice to move it, its corner to resize; edges snap to the output's and other
     slices' edges. The buffer changes on release. -->
<script lang="ts">
  import type { Rect } from '$lib/bindings/Rect';
  import { dragRect, fitInto, fitScale, lockAspect, newMapping, snapDrag, type DragMode, type Setup } from '$lib/logic';
  import NumField from './NumField.svelte';

  let { buf = $bindable(), outputId, mappingId = $bindable() }: { buf: Setup; outputId: string | null; mappingId: string | null } = $props();

  let cw = $state(0);
  let ch = $state(0);
  let addScreenId = $state('');

  const out = $derived(buf.outputs.find((o) => o.id === outputId));
  const maps = $derived(buf.mappings.filter((m) => m.outputId === outputId));
  const map = $derived(maps.find((m) => m.id === mappingId));
  const scale = $derived(out ? fitScale({ x: 0, y: 0, w: out.width, h: out.height }, cw - 24, ch - 24) : 0);
  const screen = (id: string) => buf.screens.find((s) => s.id === id);
  /** The screen "Add mapping" uses: the one picked, else the first. */
  const addScreen = $derived(screen(addScreenId) ?? buf.screens[0]);
  const hue = (screenId: string) => (28 + buf.screens.findIndex((s) => s.id === screenId) * 137) % 360;

  /** Resizing a slice keeps its screen region's shape (dst at the src's aspect), so nothing stretches. */
  let keepAspect = $state(true);
  const aspect = (id: string) => {
    const src = buf.mappings.find((m) => m.id === id)?.src;
    return src && src.w > 0 && src.h > 0 ? src.w / src.h : 1;
  };

  type Drag = { id: string; mode: DragMode; x0: number; y0: number; start: Rect; rect: Rect };
  let drag = $state.raw<Drag | null>(null);

  function down(e: PointerEvent, id: string, dst: Rect, mode: DragMode) {
    if (e.button !== 0) return;
    e.stopPropagation();
    mappingId = id;
    (e.currentTarget as Element).setPointerCapture(e.pointerId);
    drag = { id, mode, x0: e.clientX, y0: e.clientY, start: { ...dst }, rect: { ...dst } };
  }
  function move(e: PointerEvent) {
    if (!drag || !out || scale <= 0) return;
    const r = dragRect(drag.start, (e.clientX - drag.x0) / scale, (e.clientY - drag.y0) / scale, drag.mode);
    const others = maps.filter((m) => m.id !== drag!.id).map((m) => m.dst);
    drag = { ...drag, rect: snapDrag(r, drag.mode, others, out.width, out.height, scale, keepAspect ? aspect(drag.id) : undefined) };
  }
  function up() {
    const m = drag && buf.mappings.find((x) => x.id === drag!.id);
    if (m && drag) m.dst = drag.rect;
    drag = null;
  }

  function addMapping() {
    if (!out || !addScreen) return;
    const m = newMapping(addScreen, out);
    buf.mappings.push(m);
    mappingId = m.id;
  }
  function removeMapping() {
    buf.mappings = buf.mappings.filter((m) => m.id !== mappingId);
    mappingId = null;
  }
</script>

<section class="panel editor">
  <h2>
    Mapping
    {#if out}
      <span class="sub">{out.name} · {out.width}×{out.height} · {out.target.type === 'Display' ? out.target.monitorName || 'display' : 'window'}</span>
    {/if}
  </h2>
  {#if out}
    <div class="bar">
      <label class="text"><span>Output name</span><input value={out.name} onchange={(e) => (e.currentTarget.value = out.name = e.currentTarget.value.trim() || out.name)} /></label>
      <NumField label="W" value={out.width} int min={1} onset={(v) => (out.width = v)} />
      <NumField label="H" value={out.height} int min={1} onset={(v) => (out.height = v)} />
      {#if out.target.type === 'Window'}
        {@const t = out.target}
        <NumField label="Window X" value={t.x} int onset={(v) => (t.x = v)} />
        <NumField label="Window Y" value={t.y} int onset={(v) => (t.y = v)} />
      {/if}
    </div>
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="area" bind:clientWidth={cw} bind:clientHeight={ch} onpointerdown={() => (mappingId = null)}>
      <div
        class="stage"
        style:width="{out.width * scale}px"
        style:height="{out.height * scale}px"
        onpointermove={move}
        onpointerup={up}
        onpointercancel={() => (drag = null)}
      >
        {#each maps as m (m.id)}
          {@const r = drag?.id === m.id ? drag.rect : m.dst}
          <div
            class="slice"
            class:sel={m.id === mappingId}
            style:--hue={hue(m.screenId)}
            style:left="{r.x * scale}px"
            style:top="{r.y * scale}px"
            style:width="{r.w * scale}px"
            style:height="{r.h * scale}px"
            onpointerdown={(e) => down(e, m.id, m.dst, 'move')}
          >
            <span class="tag">{screen(m.screenId)?.name ?? '?'} <small>{m.src.x},{m.src.y} {m.src.w}×{m.src.h}</small></span>
            <div class="handle" onpointerdown={(e) => down(e, m.id, m.dst, 'resize')}></div>
          </div>
        {/each}
      </div>
    </div>
    <div class="bar">
      {#if map}
        {#key map.id}
          <span class="which">{screen(map.screenId)?.name}</span>
          <span class="grp">Screen (src)</span>
          <NumField label="X" value={map.src.x} onset={(v) => (map.src.x = v)} />
          <NumField label="Y" value={map.src.y} onset={(v) => (map.src.y = v)} />
          <NumField label="W" value={map.src.w} min={1} onset={(v) => (map.src.w = v)} />
          <NumField label="H" value={map.src.h} min={1} onset={(v) => (map.src.h = v)} />
          <span class="grp">Output (dst)</span>
          <NumField label="X" value={map.dst.x} onset={(v) => (map.dst.x = v)} />
          <NumField label="Y" value={map.dst.y} onset={(v) => (map.dst.y = v)} />
          <NumField label="W" value={map.dst.w} min={1} onset={(v) => (map.dst = keepAspect ? lockAspect({ ...map.dst, w: v }, aspect(map.id), 'w') : { ...map.dst, w: v })} />
          <NumField label="H" value={map.dst.h} min={1} onset={(v) => (map.dst = keepAspect ? lockAspect({ ...map.dst, h: v }, aspect(map.id), 'h') : { ...map.dst, h: v })} />
          <label class="check" title="Resizing keeps the screen region's shape, so the picture isn't stretched"><input type="checkbox" bind:checked={keepAspect} /> Keep aspect</label>
          <button title="As large as fits on the output at the screen region's shape, centred" onclick={() => (map.dst = fitInto(aspect(map.id), out.width, out.height))}>Fit</button>
          <button title="Fill the whole output, stretching if the shapes differ" onclick={() => (map.dst = { x: 0, y: 0, w: out.width, h: out.height })}>Stretch</button>
          <button onclick={removeMapping}>Remove mapping</button>
        {/key}
      {:else}
        <span class="muted">{maps.length ? 'Select a slice to edit it.' : 'Nothing mapped onto this output yet.'}</span>
      {/if}
      <span class="add">
        <select value={addScreen?.id} onchange={(e) => (addScreenId = e.currentTarget.value)} disabled={!buf.screens.length}>
          {#each buf.screens as s (s.id)}<option value={s.id}>{s.name}</option>{/each}
        </select>
        <button onclick={addMapping} disabled={!buf.screens.length}>Add mapping</button>
      </span>
    </div>
  {:else}
    <p class="muted empty">{buf.outputs.length ? 'Select an output.' : 'Add an output to map screens onto it.'}</p>
  {/if}
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
  .bar {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 8px;
    align-items: flex-end;
    padding: 6px 8px;
  }
  .text {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .text span,
  .grp {
    font-size: 10px;
    color: var(--muted);
  }
  .grp {
    align-self: center;
    margin-left: 6px;
  }
  .check {
    align-self: center;
    display: flex;
    align-items: center;
    gap: 4px;
    font-size: 11px;
  }
  .which {
    align-self: center;
    font-weight: 600;
  }
  .add {
    margin-left: auto;
    display: flex;
    gap: 4px;
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
    background: #000;
    outline: 1px solid var(--muted);
    touch-action: none;
  }
  .slice {
    position: absolute;
    background: hsl(var(--hue) 80% 55% / 0.18);
    outline: 1px solid hsl(var(--hue) 80% 60% / 0.8);
    cursor: move;
  }
  .slice.sel {
    outline: 2px solid var(--accent);
    background: hsl(var(--hue) 80% 55% / 0.32);
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
  .empty {
    padding: 12px;
  }
</style>
