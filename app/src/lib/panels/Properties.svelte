<script lang="ts">
  import type { Layer } from '$lib/bindings/Layer';
  import type { Playback } from '$lib/bindings/Playback';
  import type { TransitionKind } from '$lib/bindings/TransitionKind';
  import { hexToRgba, lockAspect, mediaOf, rgbaToHex } from '$lib/logic';
  import { editLayer, moveLayer, setTransition, ui } from '$lib/store.svelte';

  const kinds: TransitionKind[] = ['Cut', 'Fade', 'WipeLeft', 'WipeRight'];
  const endModes: [Playback, string][] = [
    ['Loop', 'Loop'],
    ['Once', 'Play once, then clear'],
    ['HoldLast', 'Stop on last frame'],
  ];

  const scene = $derived(ui.busScene);
  const t = $derived(ui.live.transition);
  const layer = $derived(ui.layer);
  const screens = $derived((ui.show?.screens ?? []).filter((s) => s.id in scene));
  const media = $derived(mediaOf(ui.show, layer));
  const screen = $derived(ui.show?.screens.find((s) => s.id === ui.selected?.screenId));

  /** Number from an input, or null for empty/garbage (the field then snaps back on the next update). */
  const num = (e: Event) => {
    const n = (e.currentTarget as HTMLInputElement).valueAsNumber;
    return Number.isFinite(n) ? n : null;
  };

  async function set(patch: Partial<Layer>) {
    if (ui.selected) await editLayer(ui.selected, (l) => ({ ...l, ...patch }));
  }
  /** `r` with its width or height set to `v` (at least 1); with Keep aspect on, the other side
   *  follows so the layer keeps its shape. */
  function sized(r: Layer['rect'], side: 'w' | 'h', v: number): Layer['rect'] {
    const next = { ...r, [side]: Math.max(1, v) };
    return ui.keepAspect ? lockAspect(next, r.w / r.h, side) : next;
  }

  /** Commit a number; afterwards the control shows the layer's actual value again (an on-air
   *  confirm may have been declined). */
  async function setNum(e: Event, f: (v: number) => Partial<Layer>, current: () => number) {
    const el = e.currentTarget as HTMLInputElement;
    const v = num(e);
    if (v !== null) await set(f(v));
    el.value = String(current());
  }
</script>

{#snippet slider(label: string, value: number, min: number, max: number, f: (v: number) => Partial<Layer>)}
  <label class="row">
    <span>{label}</span>
    <input type="range" {min} {max} step="0.01" {value} onchange={(e) => setNum(e, f, () => value)} />
    <span class="val">{value.toFixed(2)}</span>
  </label>
{/snippet}

{#snippet field(label: string, value: number, f: (v: number) => Partial<Layer>)}
  <label class="cell"><span>{label}</span><input type="number" {value} onchange={(e) => setNum(e, f, () => value)} /></label>
{/snippet}

<section class="panel props">
  <h2>
    Properties
    {#if ui.canvasTab === 'pgm'}<span class="badge live">ON AIR</span>{/if}
  </h2>
  <div class="body">
    {#if ui.canvasTab === 'pvw'}
      <h4>Preview <span class="muted">· {ui.pvwCue ? `from cue ${ui.pvwCue.slot + 1}${ui.pvwEdited ? ', edited' : ''}` : 'not stored in a cue'}</span></h4>
      <label class="row">
        <span>Transition</span>
        <select value={t.kind} onchange={(e) => setTransition({ ...t, kind: e.currentTarget.value as TransitionKind })}>
          {#each kinds as k (k)}<option value={k}>{k}</option>{/each}
        </select>
      </label>
      <label class="row">
        <span>Duration ms</span>
        <input
          type="number"
          min="0"
          step="50"
          value={t.durationMs}
          disabled={t.kind === 'Cut'}
          onchange={(e) => {
            const v = num(e);
            if (v !== null) setTransition({ ...t, durationMs: Math.max(0, Math.round(v)) });
          }}
        />
      </label>
    {/if}

      <h4>Layers</h4>
      {#each screens as s (s.id)}
        <div class="screen">{s.name}{#if !scene[s.id].length}<span class="muted"> · black</span>{/if}</div>
        <ul class="layers">
          {#each scene[s.id].toReversed() as l, i (l.id)}
            <li class:sel={ui.selected?.layerId === l.id}>
              <button class="pick" onclick={() => (ui.selected = { screenId: s.id, layerId: l.id })}>{l.name || l.source.type}</button>
              <!-- Listed top first: the first row is the top layer. -->
              <button class="icon" title="Move up (in front)" disabled={i === 0} onclick={() => moveLayer({ screenId: s.id, layerId: l.id }, 1)}>▲</button>
              <button class="icon" title="Move down (behind)" disabled={i === scene[s.id].length - 1} onclick={() => moveLayer({ screenId: s.id, layerId: l.id }, -1)}>▼</button>
              {#if mediaOf(ui.show, l)?.kind === 'Video'}
                <button class="icon" title={l.muted ? 'Unmute' : 'Mute'} onclick={() => editLayer({ screenId: s.id, layerId: l.id }, (x) => ({ ...x, muted: !x.muted }))}>{l.muted ? '🔇' : '🔈'}</button>
              {/if}
              <button
                class="icon"
                title={l.visible ? 'Hide' : 'Show'}
                onclick={() => editLayer({ screenId: s.id, layerId: l.id }, (x) => ({ ...x, visible: !x.visible }))}>{l.visible ? '◉' : '○'}</button
              >
              <button class="icon" title="Remove layer" onclick={() => editLayer({ screenId: s.id, layerId: l.id }, () => null)}>✕</button>
            </li>
          {/each}
        </ul>
      {:else}
        <p class="muted">{ui.canvasTab === 'pvw' ? 'No layers. Drag media or a pattern onto the canvas, or click a cue to load it.' : 'Nothing on air.'}</p>
      {/each}

      {#if layer}
        <h4>Layer</h4>
        <label class="row"><span>Name</span><input value={layer.name} onchange={(e) => set({ name: e.currentTarget.value })} /></label>
        <div class="row">
          <span>Source</span>
          {#if layer.source.type === 'Media'}
            <span class="src">{media?.name ?? 'missing media'}</span>
          {:else if layer.source.pattern.type === 'Solid'}
            <input type="color" value={rgbaToHex(layer.source.pattern.rgba)} onchange={(e) => set({ source: { type: 'Pattern', pattern: { type: 'Solid', rgba: hexToRgba(e.currentTarget.value) } } })} />
          {:else}
            <span class="src">{layer.source.pattern.type} pattern</span>
          {/if}
        </div>
        {#if ui.selected}
          {@const sel = ui.selected}
          {@const at = (scene[sel.screenId] ?? []).findIndex((l) => l.id === sel.layerId)}
          {@const top = (scene[sel.screenId] ?? []).length - 1}
          <div class="row">
            <span>Order</span>
            <span class="order">
              <button class="icon" title="To the top" disabled={at === top} onclick={() => moveLayer(sel, Infinity)}>⤒</button>
              <button class="icon" title="Up one" disabled={at === top} onclick={() => moveLayer(sel, 1)}>▲</button>
              <button class="icon" title="Down one" disabled={at === 0} onclick={() => moveLayer(sel, -1)}>▼</button>
              <button class="icon" title="To the bottom" disabled={at === 0} onclick={() => moveLayer(sel, -Infinity)}>⤓</button>
            </span>
          </div>
        {/if}
        {#if media?.kind === 'Video'}
          <label class="row">
            <span>End mode</span>
            <select value={layer.playback} onchange={(e) => set({ playback: e.currentTarget.value as Playback })}>
              {#each endModes as [p, label] (p)}<option value={p}>{label}</option>{/each}
            </select>
          </label>
          {@render slider('Volume', layer.volume, 0, 1, (volume) => ({ volume }))}
          <label class="row"><span>Mute</span><input type="checkbox" checked={layer.muted} onchange={(e) => set({ muted: e.currentTarget.checked })} /></label>
          {#if media?.durationMs}
            {@const pos = ui.positions[layer.id]}
            {@const end = media.durationMs}
            <label class="row">
              <span>Speed ×</span>
              <input type="number" min="0.25" max="4" step="0.05" value={layer.speed} onchange={(e) => setNum(e, (v) => ({ speed: Math.min(4, Math.max(0.25, v)) }), () => layer.speed)} />
            </label>
            {#if layer.speed !== 1}<p class="hint">Audio is silent unless the speed is 1×.</p>{/if}
            <div class="row">
              <span>Trim (s)</span>
              <span class="trim">
                <input
                  type="number"
                  min="0"
                  step="0.1"
                  title="In point: seconds into the clip"
                  value={layer.trimInMs / 1000}
                  onchange={(e) => setNum(e, (v) => ({ trimInMs: Math.round(Math.max(0, v) * 1000) }), () => layer.trimInMs / 1000)}
                />
                <input
                  type="number"
                  min="0"
                  step="0.1"
                  title="Out point: seconds into the clip (empty: its end)"
                  placeholder={(end / 1000).toFixed(1)}
                  value={layer.trimOutMs == null ? '' : layer.trimOutMs / 1000}
                  onchange={async (e) => {
                    const el = e.currentTarget;
                    const v = el.value.trim() === '' ? null : num(e);
                    if (v === null && el.value.trim() !== '') return void (el.value = layer.trimOutMs == null ? '' : String(layer.trimOutMs / 1000));
                    await set({ trimOutMs: v === null ? null : Math.round(v * 1000) });
                  }}
                />
              </span>
            </div>
            <div class="row">
              <span></span>
              <span class="trim">
                <button disabled={pos == null} title="In point at the clip's current position" onclick={() => pos != null && set({ trimInMs: pos })}>Set in</button>
                <button disabled={pos == null} title="Out point at the clip's current position" onclick={() => pos != null && set({ trimOutMs: pos })}>Set out</button>
                <button disabled={!layer.trimInMs && layer.trimOutMs == null} onclick={() => set({ trimInMs: 0, trimOutMs: null })}>Clear</button>
              </span>
            </div>
          {/if}
        {/if}
        <label class="row">
          <span>Rotation °</span>
          <input type="number" step="1" value={layer.rotation} onchange={(e) => setNum(e, (rotation) => ({ rotation }), () => layer.rotation)} />
        </label>
        <div class="grid">
          {@render field('X', layer.rect.x, (x) => ({ rect: { ...layer.rect, x } }))}
          {@render field('Y', layer.rect.y, (y) => ({ rect: { ...layer.rect, y } }))}
          {@render field('W', layer.rect.w, (w) => ({ rect: sized(layer.rect, 'w', w) }))}
          {@render field('H', layer.rect.h, (h) => ({ rect: sized(layer.rect, 'h', h) }))}
        </div>
        {#if screen}
          <button class="fit" onclick={() => set({ rect: { x: 0, y: 0, w: screen.width, h: screen.height } })}>Fit to {screen.name}</button>
        {/if}
        {@render slider('Opacity', layer.opacity, 0, 1, (opacity) => ({ opacity }))}
        {@render slider('Brightness', layer.brightness, -1, 1, (brightness) => ({ brightness }))}
        {@render slider('Contrast', layer.contrast, 0, 2, (contrast) => ({ contrast }))}
        {@render slider('Saturation', layer.saturation, 0, 2, (saturation) => ({ saturation }))}
        <label class="row"><span>Visible</span><input
            type="checkbox"
            checked={layer.visible}
            onchange={async (e) => {
              const el = e.currentTarget;
              await set({ visible: el.checked });
              el.checked = ui.layer?.visible ?? el.checked; // a declined on-air confirm leaves it as it was
            }}
          /></label
        >
      {/if}
  </div>
</section>

<style>
  .props {
    flex: 1;
  }
  .body {
    padding: 6px 8px 10px;
    overflow-y: auto;
    flex: 1;
  }
  h4 {
    margin: 10px 0 4px;
    font-size: 11px;
    color: var(--accent-2);
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }
  h4:first-child {
    margin-top: 0;
  }
  .row {
    display: grid;
    grid-template-columns: 78px 1fr auto;
    align-items: center;
    gap: 6px;
    margin: 3px 0;
  }
  .row > span:first-child,
  .cell > span {
    color: var(--muted);
  }
  .val {
    width: 34px;
    text-align: right;
    font-variant-numeric: tabular-nums;
    color: var(--muted);
  }
  .src {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 4px 8px;
    margin: 4px 0;
  }
  .cell {
    display: grid;
    grid-template-columns: 14px 1fr;
    align-items: center;
    gap: 4px;
  }
  .fit {
    margin: 2px 0 6px;
    font-size: 11px;
  }
  .screen {
    margin-top: 4px;
    font-weight: 600;
  }
  .layers {
    list-style: none;
    margin: 2px 0;
    padding: 0;
  }
  .layers li {
    display: flex;
    gap: 2px;
  }
  .layers li.sel .pick {
    border-color: var(--accent);
    background: var(--accent-bg);
  }
  .pick {
    flex: 1;
    text-align: left;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .icon {
    padding: 2px 6px;
  }
  .order {
    display: flex;
    gap: 3px;
  }
  .trim {
    display: flex;
    gap: 4px;
    min-width: 0;
  }
  .trim input {
    width: 0;
    flex: 1;
  }
  .trim button {
    padding: 1px 6px;
    font-size: 11px;
  }
  .hint {
    margin: 0 0 4px;
    font-size: 11px;
    color: var(--muted);
  }
</style>
