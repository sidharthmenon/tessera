<script lang="ts">
  import type { TransportAction } from '$lib/bindings/TransportAction';
  import { cueAt, fmtTime, mediaOf } from '$lib/logic';
  import { loadNext, op, take, transport, ui } from '$lib/store.svelte';

  const bus = $derived(ui.canvasTab);
  /** As the bus has it: on PGM this reaches layers earlier cues left on air too (R41). */
  const layer = $derived(ui.busLayer);
  const media = $derived(mediaOf(ui.show, layer));
  /** A pattern has no player to drive; with nothing selected, transport drives the whole bus. */
  const usable = $derived(!!ui.engine && (!layer || layer.source.type === 'Media'));
  const pos = $derived(layer ? ui.positions[layer.id] : undefined);
  const pvwCue = $derived(cueAt(ui.show, ui.live.pvw));
  const pgmCue = $derived(cueAt(ui.show, ui.live.lastTaken));

  // The slider moves freely; one SetMasterVolume (one undo step) on release.
  let dragVol = $state<number | null>(null);
  const vol = $derived(dragVol ?? ui.show?.masterVolume ?? 1);

  const buttons: [TransportAction, string][] = [
    ['Play', '▶'],
    ['Pause', '❚❚'],
    ['Stop', '■'],
    ['Restart', '⟲'],
  ];
</script>

<section class="transport">
  <div class="tp">
    <span class="target">
      <span class="badge" class:live={bus === 'pgm'} class:pvw={bus === 'pvw'}>{bus.toUpperCase()}</span>
      {layer ? layer.name || 'layer' : 'all layers'}
    </span>
    {#each buttons as [act, icon] (act)}
      <button
        disabled={!usable}
        class:warn={bus === 'pgm' && (act === 'Stop' || act === 'Restart')}
        title={bus === 'pgm' && (act === 'Stop' || act === 'Restart') ? `${act} on air: blanks the picture until the first frame (asks first)` : act}
        onclick={() => transport(bus, layer?.id ?? null, act)}>{icon}</button
      >
    {/each}
    {#if bus === 'pvw' && ui.pvwLive}<span class="onair" title="Preview never touches a clip that is on air">live clips: use Program</span>{/if}
    <span class="pos">{pos != null ? fmtTime(pos) : '–:–'}{media?.durationMs != null ? ` / ${fmtTime(media.durationMs)}` : ''}</span>
  </div>

  <label class="vol">
    <span>Master</span>
    <input
      type="range"
      min="0"
      max="1"
      step="0.01"
      value={vol}
      oninput={(e) => (dragVol = e.currentTarget.valueAsNumber)}
      onchange={async (e) => {
        await op({ op: 'SetMasterVolume', volume: e.currentTarget.valueAsNumber });
        dragVol = null;
      }}
    />
    <span class="val">{Math.round(vol * 100)}%</span>
  </label>

  <div class="go">
    <div class="cues">
      <span><span class="badge pvw">PVW</span> {pvwCue ? `${pvwCue.slot + 1} · ${pvwCue.name}` : '—'}</span>
      <span><span class="badge live">PGM</span> {pgmCue ? `${pgmCue.slot + 1} · ${pgmCue.name}` : '—'}</span>
    </div>
    <button onclick={() => loadNext()} title="Load the next cue into Preview (→)">Next ▸</button>
    <button class="primary big" disabled={!pvwCue} onclick={() => take()} title="Take Preview to Program (Space)">GO</button>
  </div>
</section>

<style>
  .transport {
    grid-area: transport;
    display: flex;
    align-items: center;
    gap: 18px;
    padding: 6px 10px;
    background: var(--panel);
    border-top: 1px solid var(--border);
    border-bottom: 1px solid var(--border);
  }
  .tp {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .target {
    min-width: 150px;
    max-width: 220px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    margin-right: 6px;
  }
  .tp button {
    width: 34px;
  }
  .tp button.warn {
    color: var(--warn);
  }
  .onair {
    margin-left: 6px;
    color: var(--warn);
    font-size: 11px;
  }
  .pos {
    margin-left: 8px;
    font-variant-numeric: tabular-nums;
    color: var(--muted);
  }
  .vol {
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--muted);
  }
  .vol input {
    width: 140px;
  }
  .val {
    width: 34px;
    font-variant-numeric: tabular-nums;
  }
  .go {
    margin-left: auto;
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .cues {
    display: flex;
    flex-direction: column;
    gap: 2px;
    max-width: 260px;
    white-space: nowrap;
    overflow: hidden;
  }
  .big {
    font-size: 18px;
    font-weight: 800;
    letter-spacing: 0.1em;
    padding: 8px 30px;
  }
</style>
