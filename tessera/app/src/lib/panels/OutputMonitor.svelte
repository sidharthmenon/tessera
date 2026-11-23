<script lang="ts">
  import { engineUrl, layoutBounds } from '$lib/logic';
  import { ui } from '$lib/store.svelte';

  // While the canvas shows Program it is the same stream, and the engine serves one viewer per bus.
  const onCanvas = $derived(ui.canvasTab === 'pgm');
  const url = $derived(onCanvas ? null : engineUrl(ui.engine, '/preview/pgm.mjpg'));
  const b = $derived(layoutBounds(ui.show?.screens ?? []));
  const bound = (id: string) => ui.outputs.find((o) => o.outputId === id);
</script>

<section class="panel monitor">
  <h2>Output monitor <span class="badge live">PGM</span></h2>
  <div class="frame" style:aspect-ratio="{b.w} / {b.h}">
    {#if url && ui.show?.screens.length}
      {#key url}<img src={url} alt="Program output" />{/key}
    {:else if onCanvas}
      <span class="muted">on the canvas</span>
    {/if}
  </div>
  {#if ui.show?.outputs.length}
    <ul>
      {#each ui.show.outputs as o (o.id)}
        {@const st = bound(o.id)}
        <li title={st?.monitor ?? ''}>
          <span class="dot" class:ok={st?.bound}></span>{o.name}
          <span class="muted">{o.width}×{o.height} · {st ? (st.bound ? (st.monitor ?? 'bound') : 'unbound') : '–'}</span>
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
  .monitor {
    flex: none;
  }
  .frame {
    margin: 6px;
    max-height: 190px;
    background: #000;
    border: 1px solid var(--border);
    display: grid;
    place-items: center;
    font-size: 11px;
  }
  .frame img {
    width: 100%;
    height: 100%;
    object-fit: contain;
    display: block;
  }
  ul {
    list-style: none;
    margin: 0 6px 6px;
    padding: 0;
    font-size: 11px;
  }
  li {
    display: flex;
    gap: 6px;
    align-items: center;
    white-space: nowrap;
    overflow: hidden;
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--muted);
  }
  .dot.ok {
    background: var(--pvw);
  }
</style>
