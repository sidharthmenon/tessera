<script lang="ts">
  import type { MediaItem } from '$lib/bindings/MediaItem';
  import type { Pattern } from '$lib/bindings/Pattern';
  import type { Source } from '$lib/bindings/Source';
  import { engineUrl, fmtTime, hexToRgba } from '$lib/logic';
  import { addToPvw, importMedia, ui } from '$lib/store.svelte';

  let tab = $state<'media' | 'patterns'>('media');
  let solid = $state('#ff8a1f');

  const patterns = $derived<{ name: string; pattern: Pattern }[]>([
    { name: 'Color bars', pattern: { type: 'Bars' } },
    { name: 'Grid', pattern: { type: 'Grid' } },
    { name: 'Gradient', pattern: { type: 'Gradient' } },
    { name: 'Solid', pattern: { type: 'Solid', rgba: hexToRgba(solid) } },
  ]);

  const media = (m: MediaItem): Source => ({ type: 'Media', mediaId: m.id });
  const pattern = (p: Pattern): Source => ({ type: 'Pattern', pattern: p });

  function meta(m: MediaItem) {
    if (!m.width) return 'probing…';
    const parts = [`${m.width}×${m.height}`];
    if (m.durationMs != null) parts.push(fmtTime(m.durationMs));
    if (m.hasAudio) parts.push('audio');
    if (m.hasAlpha) parts.push('alpha');
    return parts.join(' · ');
  }

  function dragStart(e: DragEvent, source: Source, name: string) {
    ui.dragging = { source, name };
    e.dataTransfer?.setData('text/plain', name);
    if (e.dataTransfer) e.dataTransfer.effectAllowed = 'copy';
  }
</script>

{#snippet row(source: Source, name: string, sub: string, thumb: string | null, icon: string)}
  <li draggable="true" ondragstart={(e) => dragStart(e, source, name)} ondragend={() => (ui.dragging = null)} title="Drag onto a screen in the canvas to fill it, or onto a layer to replace it">
    <span class="thumb">
      {#if thumb}<img src={thumb} alt="" draggable="false" />{:else}{icon}{/if}
    </span>
    <span class="text">
      <span class="name">{name}</span>
      <span class="sub">{sub}</span>
    </span>
    <button class="add" onclick={() => addToPvw(source, name)} title="Add to the Preview cue, filling the highlighted screen (click a screen on the canvas to pick it)">+ PVW</button>
  </li>
{/snippet}

<section class="panel resources">
  <div class="panel-head">
    <div class="tabs">
      <button class:on={tab === 'media'} onclick={() => (tab = 'media')}>Local media</button>
      <button class:on={tab === 'patterns'} onclick={() => (tab = 'patterns')}>Patterns</button>
    </div>
    {#if tab === 'media'}<button class="import" onclick={() => importMedia()}>Import…</button>{/if}
  </div>
  <ul>
    {#if tab === 'media'}
      {#each ui.show?.media ?? [] as m (m.id)}
        {@render row(media(m), m.name, meta(m), m.thumb ? engineUrl(ui.engine, `/thumb/${encodeURIComponent(m.thumb)}`) : null, m.kind === 'Video' ? '▶' : '▣')}
      {:else}
        <p class="muted empty">No media yet. Import images or videos.</p>
      {/each}
    {:else}
      {#each patterns as p (p.name)}
        {@render row(pattern(p.pattern), p.name, 'test pattern', null, '▦')}
      {/each}
      <li class="solid"><label>Solid colour <input type="color" bind:value={solid} /></label></li>
    {/if}
  </ul>
</section>

<style>
  .resources {
    grid-area: res;
  }
  .import {
    margin-left: auto;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 4px;
    overflow-y: auto;
    flex: 1;
  }
  li {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px;
    border-radius: var(--radius);
    cursor: grab;
  }
  li:hover {
    background: var(--panel-2);
  }
  .thumb {
    flex: none;
    width: 56px;
    height: 32px;
    background: var(--bg);
    border: 1px solid var(--border);
    display: grid;
    place-items: center;
    color: var(--muted);
    overflow: hidden;
  }
  .thumb img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    /* A checkerboard shows through where the media is transparent. */
    background: repeating-conic-gradient(#3a3d42 0 25%, #24262a 0 50%) 0 0 / 8px 8px;
  }
  .text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .name,
  .sub {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .sub {
    color: var(--muted);
    font-size: 11px;
  }
  .add {
    padding: 1px 6px;
    font-size: 11px;
    opacity: 0;
  }
  li:hover .add,
  .add:focus-visible {
    opacity: 1;
  }
  .solid {
    cursor: default;
    color: var(--muted);
  }
  .solid input {
    vertical-align: middle;
    margin-left: 6px;
    width: 36px;
    height: 20px;
    border: none;
    background: none;
  }
  .empty {
    padding: 8px;
  }
</style>
