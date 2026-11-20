<script lang="ts">
  import type { Layer } from '$lib/bindings/Layer';
  import type { Playback } from '$lib/bindings/Playback';
  import type { TransitionKind } from '$lib/bindings/TransitionKind';
  import { hexToRgba, mediaOf, rgbaToHex } from '$lib/logic';
  import { editCue, editLayer, ui } from '$lib/store.svelte';

  const kinds: TransitionKind[] = ['Cut', 'Fade', 'WipeLeft', 'WipeRight'];
  const playbacks: Playback[] = ['Loop', 'Once', 'HoldLast'];

  const cue = $derived(ui.editCue);
  const ref = $derived(ui.editRef);
  const layer = $derived(ui.layer);
  const screens = $derived((ui.show?.screens ?? []).filter((s) => cue && s.id in cue.layersByScreen));
  const media = $derived(mediaOf(ui.show, layer));
  const screen = $derived(ui.show?.screens.find((s) => s.id === ui.selected?.screenId));

  /** Number from an input, or null for empty/garbage (the field then snaps back on the next update). */
  const num = (e: Event) => {
    const n = (e.currentTarget as HTMLInputElement).valueAsNumber;
    return Number.isFinite(n) ? n : null;
  };

  function set(patch: Partial<Layer>) {
    if (ui.selected) editLayer(ui.selected, (l) => ({ ...l, ...patch }));
  }
  function setNum(e: Event, f: (v: number) => Partial<Layer>) {
    const v = num(e);
    if (v !== null) set(f(v));
  }
</script>

{#snippet slider(label: string, value: number, min: number, max: number, f: (v: number) => Partial<Layer>)}
  <label class="row">
    <span>{label}</span>
    <input type="range" {min} {max} step="0.01" {value} onchange={(e) => setNum(e, f)} />
    <span class="val">{value.toFixed(2)}</span>
  </label>
{/snippet}

{#snippet field(label: string, value: number, f: (v: number) => Partial<Layer>)}
  <label class="cell"><span>{label}</span><input type="number" {value} onchange={(e) => setNum(e, f)} /></label>
{/snippet}

<section class="panel props">
  <h2>Properties {#if ui.canvasTab === 'pgm' || ui.pvwLive}<span class="badge live">ON AIR</span>{/if}</h2>
  <div class="body">
    {#if cue && ref}
      <h4>Cue {cue.slot + 1}</h4>
      <label class="row"><span>Name</span><input value={cue.name} onchange={(e) => editCue(ref, (c) => ({ ...c, name: e.currentTarget.value }))} /></label>
      <label class="row">
        <span>Transition</span>
        <select
          value={cue.transition.kind}
          onchange={(e) => {
            const kind = e.currentTarget.value as TransitionKind;
            editCue(ref, (c) => ({ ...c, transition: { ...c.transition, kind } }));
          }}
        >
          {#each kinds as k (k)}<option value={k}>{k}</option>{/each}
        </select>
      </label>
      <label class="row">
        <span>Duration ms</span>
        <input
          type="number"
          min="0"
          step="50"
          value={cue.transition.durationMs}
          disabled={cue.transition.kind === 'Cut'}
          onchange={(e) => {
            const v = num(e);
            if (v !== null) editCue(ref, (c) => ({ ...c, transition: { ...c.transition, durationMs: Math.max(0, Math.round(v)) } }));
          }}
        />
      </label>

      <h4>Layers</h4>
      {#each screens as s (s.id)}
        <div class="screen">{s.name}{#if !cue.layersByScreen[s.id].length}<span class="muted"> · black</span>{/if}</div>
        <ul class="layers">
          {#each cue.layersByScreen[s.id].toReversed() as l (l.id)}
            <li class:sel={ui.selected?.layerId === l.id}>
              <button class="pick" onclick={() => (ui.selected = { screenId: s.id, layerId: l.id })}>{l.name || l.source.type}</button>
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
        <p class="muted">No layers. Drag media or a pattern onto the canvas.</p>
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
        <div class="grid">
          {@render field('X', layer.rect.x, (x) => ({ rect: { ...layer.rect, x } }))}
          {@render field('Y', layer.rect.y, (y) => ({ rect: { ...layer.rect, y } }))}
          {@render field('W', layer.rect.w, (w) => ({ rect: { ...layer.rect, w: Math.max(1, w) } }))}
          {@render field('H', layer.rect.h, (h) => ({ rect: { ...layer.rect, h: Math.max(1, h) } }))}
        </div>
        {#if screen}
          <button class="fit" onclick={() => set({ rect: { x: 0, y: 0, w: screen.width, h: screen.height } })}>Fit to {screen.name}</button>
        {/if}
        {@render slider('Opacity', layer.opacity, 0, 1, (opacity) => ({ opacity }))}
        {@render slider('Brightness', layer.brightness, -1, 1, (brightness) => ({ brightness }))}
        {@render slider('Contrast', layer.contrast, 0, 2, (contrast) => ({ contrast }))}
        {@render slider('Saturation', layer.saturation, 0, 2, (saturation) => ({ saturation }))}
        {#if layer.source.type === 'Media'}
          {@render slider('Volume', layer.volume, 0, 1, (volume) => ({ volume }))}
          <label class="row">
            <span>Playback</span>
            <select
              value={layer.playback}
              onchange={(e) => set({ playback: e.currentTarget.value as Playback })}
            >
              {#each playbacks as p (p)}<option value={p}>{p}</option>{/each}
            </select>
          </label>
        {/if}
        <label class="row"><span>Visible</span><input type="checkbox" checked={layer.visible} onchange={(e) => set({ visible: e.currentTarget.checked })} /></label>
      {/if}
    {:else}
      <p class="muted">{ui.canvasTab === 'pvw' ? 'Load a cue into Preview (click a cue), or drag media onto the canvas.' : 'No taken cue to edit.'}</p>
    {/if}
    {#if !layer && ui.busLayer}
      <p class="note">“{ui.busLayer.name || 'layer'}” is on air from an earlier cue: transport reaches it; edit it in its own cue.</p>
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
  .note {
    color: var(--warn);
  }
  .icon {
    padding: 2px 6px;
  }
</style>
