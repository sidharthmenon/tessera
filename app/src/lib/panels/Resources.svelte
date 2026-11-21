<script lang="ts">
  import type { MediaItem } from '$lib/bindings/MediaItem';
  import type { Pattern } from '$lib/bindings/Pattern';
  import type { Source } from '$lib/bindings/Source';
  import type { InputInfo } from '$lib/bindings/InputInfo';
  import { engineUrl, fmtTime, hexToRgba, mediaUses, uuid } from '$lib/logic';
  import { addToPvw, confirmAsk, importMedia, listInputs, op, ui } from '$lib/store.svelte';

  let tab = $state<'media' | 'inputs' | 'patterns'>('media');
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
    if (m.kind === 'Capture') return `live input${m.width ? ` · ${m.width}×${m.height}` : ''}`;
    if (m.kind === 'Ndi') return 'NDI® source';
    if (!m.width) return 'probing…';
    const parts = [`${m.width}×${m.height}`];
    if (m.durationMs != null) parts.push(fmtTime(m.durationMs));
    if (m.hasAudio) parts.push('audio');
    if (m.hasAlpha) parts.push('alpha');
    return parts.join(' · ');
  }

  const uses = $derived(mediaUses(ui.show, ui.live));
  const unused = $derived((ui.show?.media ?? []).filter((m) => !uses.has(m.id)));
  const n = (k: number, what: string) => `${k} ${what}${k === 1 ? '' : 's'}`;

  let menu = $state<{ x: number; y: number; item: MediaItem } | null>(null);
  function openMenu(e: MouseEvent, item: MediaItem) {
    e.preventDefault();
    menu = { x: e.clientX, y: e.clientY, item };
    ui.modal = 'menu';
  }
  function closeMenu() {
    menu = null;
    if (ui.modal === 'menu') ui.modal = null;
  }

  /** Removes `item`; when cues, Preview or Program use it, asks first (their layers go with it). */
  async function remove(item: MediaItem) {
    const u = uses.get(item.id);
    if (u) {
      const where = [u.cues && `${n(u.cues, 'cue')}`, u.pvw && 'Preview', u.pgm && 'Program'].filter(Boolean).join(', ');
      const air = u.pgm ? ' It disappears from Program now.' : '';
      if (!(await confirmAsk(`“${item.name}” is used by ${where}. Removing it removes those layers.${air} (Undo brings it back.)`, 'Remove'))) return;
    }
    op({ op: 'RemoveMedia', ids: [item.id] });
  }

  async function removeUnused() {
    const ids = unused.map((m) => m.id);
    if (ids.length && (await confirmAsk(`Remove ${n(ids.length, 'media item')} that no cue, Preview or Program uses? (Undo brings them back.)`, 'Remove'))) {
      op({ op: 'RemoveMedia', ids });
    }
  }

  /** The media item already made for input `i`, if any. */
  const added = (i: InputInfo) => ui.show?.media.find((m) => m.kind === i.kind && m.path === i.id);
  function addInput(i: InputInfo) {
    const item: MediaItem = { id: uuid(), name: i.name, path: i.id, kind: i.kind, width: i.width, height: i.height, durationMs: null, hasAudio: false, hasAlpha: false, thumb: null };
    op({ op: 'AddMedia', items: [item] });
  }
  function showInputs() {
    tab = 'inputs';
    listInputs();
  }

  function dragStart(e: DragEvent, source: Source, name: string) {
    ui.dragging = { source, name };
    e.dataTransfer?.setData('text/plain', name);
    if (e.dataTransfer) e.dataTransfer.effectAllowed = 'copy';
  }
</script>

<svelte:window
  onclick={closeMenu}
  onkeydown={(e) => {
    if (menu && e.key === 'Escape') closeMenu();
  }}
/>

{#snippet row(source: Source, name: string, sub: string, thumb: string | null, icon: string, item?: MediaItem)}
  <li
    draggable="true"
    class:unused={item && !uses.has(item.id)}
    ondragstart={(e) => dragStart(e, source, name)}
    ondragend={() => (ui.dragging = null)}
    oncontextmenu={(e) => item && openMenu(e, item)}
    title="Drag onto a screen in the canvas to fill it, or onto a layer to replace it{item ? ' · right-click: remove' : ''}"
  >
    <span class="thumb">
      {#if thumb}<img src={thumb} alt="" draggable="false" />{:else}{icon}{/if}
    </span>
    <span class="text">
      <span class="name">{name}</span>
      <span class="sub">{sub}{#if item && !uses.has(item.id)}<span class="tag"> · unused</span>{/if}</span>
    </span>
    <button class="add" onclick={() => addToPvw(source, name)} title="Add to the Preview cue, filling the highlighted screen (click a screen on the canvas to pick it)">+ PVW</button>
  </li>
{/snippet}

<section class="panel resources">
  <div class="panel-head">
    <div class="tabs">
      <button class:on={tab === 'media'} onclick={() => (tab = 'media')}>Media</button>
      <button class:on={tab === 'inputs'} onclick={showInputs}>Inputs</button>
      <button class:on={tab === 'patterns'} onclick={() => (tab = 'patterns')}>Patterns</button>
    </div>
  </div>
  {#if tab !== 'patterns'}
    <div class="tools">
      {#if tab === 'media'}
        <button onclick={() => importMedia()}>Import…</button>
      {:else}
        <span class="muted">Capture devices and NDI®</span>
        <button onclick={() => listInputs()} title="Look for capture devices again">Refresh</button>
      {/if}
    </div>
  {/if}
  <ul>
    {#if tab === 'media'}
      {#each ui.show?.media ?? [] as m (m.id)}
        {@render row(media(m), m.name, meta(m), m.thumb ? engineUrl(ui.engine, `/thumb/${encodeURIComponent(m.thumb)}`) : null, { Video: '▶', Image: '▣', Capture: '◉', Ndi: '⦿' }[m.kind], m)}
      {:else}
        <p class="muted empty">No media yet. Import images or videos.</p>
      {/each}
    {:else if tab === 'inputs'}
      {#if !ui.inputs}
        <p class="muted empty">Looking for capture devices…</p>
      {:else}
        {#each ui.inputs.list as i (i.id)}
          {@const item = added(i)}
          <li class="input">
            <span class="thumb">{i.kind === 'Ndi' ? '⦿' : '◉'}</span>
            <span class="text">
              <span class="name">{i.name}</span>
              <span class="sub">{i.kind === 'Ndi' ? 'NDI® source' : i.width ? `${i.width}×${i.height}` : 'capture device'}</span>
            </span>
            {#if item}
              <span class="tag" title="In Media: drag it onto a screen from there">added</span>
            {:else}
              <button onclick={() => addInput(i)} title="Add to Media as a live input">Add</button>
            {/if}
          </li>
        {:else}
          <p class="muted empty">No capture devices or NDI sources found. Connect one and press Refresh.</p>
        {/each}
        {#if ui.inputs.error}<p class="note empty">{ui.inputs.error}</p>{/if}
      {/if}
    {:else}
      {#each patterns as p (p.name)}
        {@render row(pattern(p.pattern), p.name, 'test pattern', null, '▦')}
      {/each}
      <li class="solid"><label>Solid colour <input type="color" bind:value={solid} /></label></li>
    {/if}
  </ul>
  {#if tab === 'media' && unused.length}
    <div class="foot">
      <span class="muted">{n(unused.length, 'item')} unused</span>
      <button onclick={removeUnused} title="Remove every media item no cue, Preview or Program uses">Remove unused</button>
    </div>
  {/if}
</section>

{#if menu}
  {@const item = menu.item}
  <div class="menu" class:up={menu.y > innerHeight / 2} style:left="{menu.x}px" style:top="{menu.y}px">
    <button onclick={() => addToPvw(media(item), item.name)}>Add to Preview</button>
    <button onclick={() => remove(item)}>Remove{uses.has(item.id) ? '…' : ''}</button>
  </div>
{/if}

<style>
  .resources {
    grid-area: res;
  }
  .tools {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: 6px;
    padding: 4px 8px 0;
    font-size: 11px;
  }
  .tools button {
    padding: 1px 8px;
    font-size: 11px;
  }
  .tools .muted {
    margin-right: auto;
  }
  .unused {
    opacity: 0.6;
  }
  .input {
    cursor: default;
  }
  .input button {
    padding: 1px 8px;
    font-size: 11px;
  }
  .note {
    color: var(--muted);
    font-size: 11px;
  }
  .foot {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 6px;
    padding: 4px 8px;
    border-top: 1px solid var(--border);
    font-size: 11px;
  }
  .foot button {
    padding: 1px 7px;
    font-size: 11px;
  }
  .tag {
    color: var(--accent);
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
