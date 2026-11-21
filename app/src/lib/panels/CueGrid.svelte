<script lang="ts">
  import type { Cue } from '$lib/bindings/Cue';
  import { cueKey, engineUrl, firstEmptySlot, layoutBounds, mediaOf, patternFill, sameRef, slotCount, withScreen } from '$lib/logic';
  import { addGroup, clickSlot, confirmAsk, editCue, fire, op, promptText, ui, updateCue } from '$lib/store.svelte';
  import type { CueRef } from '$lib/api';

  const group = $derived(ui.group);
  const slots = $derived.by(() => {
    if (!group) return [];
    const bySlot = new Map(group.cues.map((c) => [c.slot, c]));
    return Array.from({ length: slotCount(group) }, (_, slot) => ({ slot, cue: bySlot.get(slot) }));
  });
  const ref = (slot: number): CueRef => ({ groupId: group?.id ?? '', slot });
  const screens = $derived(ui.show?.screens ?? []);
  const bounds = $derived(layoutBounds(screens));
  /** Colour bars' stops, hard-edged: [offset, colour] (layer.wgsl's SMPTE order). */
  const bars = ['#fff', '#ff0', '#0ff', '#0f0', '#f0f', '#f00', '#00f', '#000'].flatMap((c, i) => [[i / 8, c], [(i + 1) / 8, c]] as const);

  let menu = $state<{ x: number; y: number; cue: Cue } | null>(null);
  function openMenu(e: MouseEvent, cue: Cue | undefined) {
    e.preventDefault();
    if (!cue) return;
    menu = { x: e.clientX, y: e.clientY, cue };
    ui.modal = 'menu';
  }
  function closeMenu() {
    menu = null;
    if (ui.modal === 'menu') ui.modal = null; // a Rename prompt may have taken over
  }

  // A double-click on an empty slot stores Preview there with its first click: it must not also go to air.
  let created = { slot: -1, at: 0 };
  function click(slot: number, cue: Cue | undefined) {
    if (!cue) created = { slot, at: performance.now() };
    clickSlot(ref(slot));
  }
  function dblclick(slot: number) {
    if (created.slot === slot && performance.now() - created.at < 1500) return;
    fire(ref(slot));
  }

  /** Runs `run` at once, unless `cue` is the one on air: then only after the operator agrees. */
  async function onAir(cue: Cue, message: string, okLabel: string, run: () => void) {
    if (!sameRef(ui.live.lastTaken, ref(cue.slot)) || (await confirmAsk(`Cue ${cue.slot + 1} “${cue.name}” is on air. ${message}`, okLabel))) run();
  }
  const deleteCue = (cue: Cue) =>
    onAir(cue, 'Deleting removes it from the show, not from air: Program keeps showing it until the next GO. (Undo brings it back.)', 'Delete', () =>
      op({ op: 'DeleteCue', groupId: group?.id ?? '', slot: cue.slot }),
    );
  // Cue edits never reach air (or Preview): only a load and a GO do.
  const clearScreen = (cue: Cue, s: { id: string }) => editCue(ref(cue.slot), (c) => ({ ...c, layersByScreen: withScreen(c.layersByScreen, s.id, []) }));
  const leaveScreen = (cue: Cue, s: { id: string }) => editCue(ref(cue.slot), (c) => ({ ...c, layersByScreen: withScreen(c.layersByScreen, s.id, null) }));

  // Shortcut dialog: the next key press becomes the cue's shortcut (ui.modal keeps it from the show).
  let keyFor = $state<Cue | null>(null);
  let keyNote = $state('');
  function askKey(cue: Cue) {
    keyFor = cue;
    keyNote = '';
    ui.modal = 'prompt';
  }
  function closeKey() {
    keyFor = null;
    if (ui.modal === 'prompt') ui.modal = null;
  }
  /** Sets `cue`'s shortcut; a key another cue in the group has moves here. */
  async function setKey(cue: Cue, key: string | null) {
    const other = key ? group?.cues.find((c) => c.slot !== cue.slot && c.hotkey === key) : undefined;
    if (other) await editCue(ref(other.slot), (c) => ({ ...c, hotkey: null }));
    await editCue(ref(cue.slot), (c) => ({ ...c, hotkey: key }));
  }
  function keyPressed(e: KeyboardEvent) {
    const cue = keyFor;
    if (!cue) return;
    e.preventDefault();
    e.stopPropagation();
    if (e.key === 'Escape') return closeKey();
    if (e.key === 'Backspace' || e.key === 'Delete') return void (setKey(cue, null), closeKey());
    const k = e.metaKey || e.ctrlKey || e.altKey || e.shiftKey ? null : cueKey(e);
    if (k) return void (setKey(cue, k), closeKey());
    if (!['Shift', 'Control', 'Alt', 'Meta'].includes(e.key)) keyNote = `“${e.key === ' ' ? 'Space' : e.key}” can't be a shortcut: use A–Z, 0–9 or F1–F12, without modifiers.`;
  }

  async function renameCue(cue: Cue) {
    const name = await promptText(`Rename cue ${cue.slot + 1}`, cue.name);
    if (name) editCue(ref(cue.slot), (c) => ({ ...c, name }));
  }

  async function newGroup() {
    const name = await promptText('New group', `Group ${(ui.show?.groups.length ?? 0) + 1}`);
    if (name) addGroup(name);
  }
  async function renameGroup() {
    if (!group) return;
    const id = group.id;
    const name = await promptText('Rename group', group.name);
    if (name) op({ op: 'RenameGroup', id, name });
  }
  async function removeGroup() {
    if (!group) return;
    const { id, name, cues } = group;
    if (await confirmAsk(`Remove group "${name}" and its ${cues.length} cue(s)? (Undo brings it back.)`, 'Remove')) op({ op: 'RemoveGroup', id });
  }
  const has = (g: { id: string }, r: CueRef | null) => r?.groupId === g.id;
</script>

<svelte:window
  onclick={closeMenu}
  onkeydown={(e) => {
    if (keyFor) return keyPressed(e);
    if (menu && e.key === 'Escape') closeMenu();
  }}
/>

{#snippet thumb(cue: Cue)}
  <!-- The cue's layers at their screens' layout positions, as Preview would show them (media by
       their thumbnails; brightness/contrast/saturation not drawn). A screen the cue leaves
       untouched is only outlined; one it defines is black under its layers. -->
  <svg class="thumb" viewBox="{bounds.x} {bounds.y} {bounds.w} {bounds.h}" aria-hidden="true">
    {#each screens as s (s.id)}
      {@const layers = cue.layersByScreen[s.id]}
      <svg x={s.layoutX} y={s.layoutY} width={s.width} height={s.height} viewBox="0 0 {s.width} {s.height}">
        {#if layers}<rect width={s.width} height={s.height} fill="#000" />{/if}
        {#each layers?.filter((l) => l.visible) ?? [] as l (l.id)}
          {@const m = mediaOf(ui.show, l)}
          {@const href = m?.thumb ? engineUrl(ui.engine, `/thumb/${encodeURIComponent(m.thumb)}`) : null}
          {#if href}
            <image {href} x={l.rect.x} y={l.rect.y} width={l.rect.w} height={l.rect.h} preserveAspectRatio="none" opacity={l.opacity} />
          {:else}
            <rect x={l.rect.x} y={l.rect.y} width={l.rect.w} height={l.rect.h} fill={l.source.type === 'Pattern' ? patternFill(l.source.pattern) : '#333'} opacity={l.opacity} />
          {/if}
        {/each}
        <rect class="edge" width={s.width} height={s.height} />
      </svg>
    {/each}
  </svg>
{/snippet}

<!-- Paint servers for the thumbnails' test patterns (zero-size, not display:none, which WebKit
     won't paint from). -->
<svg class="defs" width="0" height="0" aria-hidden="true">
  <defs>
    <linearGradient id="cue-bars">
      {#each bars as [offset, color], i (i)}<stop {offset} stop-color={color} />{/each}
    </linearGradient>
    <linearGradient id="cue-gradient"><stop offset="0" stop-color="#000" /><stop offset="1" stop-color="#fff" /></linearGradient>
    <pattern id="cue-grid" width="64" height="64" patternUnits="userSpaceOnUse">
      <rect width="64" height="64" fill="#000" />
      <path d="M0 0H64M0 0V64" stroke="#fff" stroke-width="6" />
    </pattern>
  </defs>
</svg>

<section class="panel cues">
  <div class="panel-head">
    <div class="tabs">
      {#each ui.show?.groups ?? [] as g (g.id)}
        <button class:on={g.id === group?.id} onclick={() => (ui.activeGroupId = g.id)} ondblclick={renameGroup}>
          {g.name}
          {#if has(g, ui.live.lastTaken)}<span class="dot live"></span>{/if}
          {#if has(g, ui.live.pvw)}<span class="dot pvw"></span>{/if}
        </button>
      {/each}
    </div>
    <button class="small" onclick={newGroup} title="Add group">+</button>
    <button class="small" onclick={renameGroup} disabled={!group}>Rename</button>
    <button class="small" onclick={removeGroup} disabled={(ui.show?.groups.length ?? 0) < 2}>Remove</button>
    <span class="hint">click: load to Preview (empty: store Preview) · double-click: load + GO · right-click: update, more</span>
  </div>
  <div class="grid">
    {#each slots as { slot, cue } (slot)}
      {@const r = ref(slot)}
      {@const pvw = sameRef(ui.live.pvw, r)}
      {@const live = sameRef(ui.live.lastTaken, r)}
      <button
        class="slot"
        class:empty={!cue}
        class:pvw
        class:live
        onclick={() => click(slot, cue)}
        ondblclick={() => dblclick(slot)}
        oncontextmenu={(e) => openMenu(e, cue)}
        title={cue ? cue.name : 'Empty: click to store Preview here'}
      >
        <span class="num">{slot + 1}</span>
        {#if cue?.hotkey}<span class="key" title="Shortcut: loads this cue into Preview">{cue.hotkey}</span>{/if}
        {#if cue}
          {@render thumb(cue)}
          <span class="cname">{cue.name}</span>
          <span class="meta">{cue.transition.kind}{cue.transition.kind !== 'Cut' ? ` ${cue.transition.durationMs}` : ''} · {Object.keys(cue.layersByScreen).length} scr</span>
        {/if}
        <span class="tags">
          {#if cue && Object.values(cue.layersByScreen).some((ls) => ls.some((l) => l.muted))}<span class="mute" title="A clip in this cue is muted">🔇</span>{/if}
          {#if live}<span class="badge live">PGM</span>{/if}
          {#if pvw}<span class="badge pvw">PVW</span>{/if}
        </span>
      </button>
    {/each}
  </div>
</section>

{#if keyFor}
  <div class="backdrop">
    <div class="dialog" role="dialog" aria-label="Cue shortcut">
      <h3>Shortcut for cue {keyFor.slot + 1} “{keyFor.name}”</h3>
      <p>Press a key: A–Z, 0–9 or F1–F12. It loads this cue into Preview while its group is open. A key another cue in this group has moves here.</p>
      <p class="muted">Now: {keyFor.hotkey ?? 'none'} · Backspace clears · Esc cancels</p>
      {#if keyNote}<p class="warn">{keyNote}</p>{/if}
      <div class="actions"><button onclick={closeKey}>Cancel</button></div>
    </div>
  </div>
{/if}

{#if menu}
  {@const cue = menu.cue}
  <div class="menu" class:up={menu.y > innerHeight / 2} class:left={menu.x > innerWidth - 240} style:left="{menu.x}px" style:top="{menu.y}px">
    {#if sameRef(ui.live.lastTaken, ref(cue.slot))}<div class="air"><span class="badge live">ON AIR</span> cue {cue.slot + 1} is on Program</div>{/if}
    <button onclick={() => updateCue(ref(cue.slot))} disabled={!Object.keys(ui.live.preview).length} title="Replace this cue's layers and transition with what Preview shows now">Update with current view</button>
    <button onclick={() => renameCue(cue)}>Rename…</button>
    <button onclick={() => askKey(cue)}>Shortcut…{cue.hotkey ? ` (${cue.hotkey})` : ''}</button>
    <button onclick={() => group && op({ op: 'DuplicateCue', groupId: group.id, from: cue.slot, to: firstEmptySlot(group, cue.slot) })}>Duplicate</button>
    <button onclick={() => deleteCue(cue)}>Delete</button>
    {#each ui.show?.screens ?? [] as s (s.id)}
      <hr />
      <button onclick={() => clearScreen(cue, s)}>Clear {s.name} (black)</button>
      {#if s.id in cue.layersByScreen}
        <button onclick={() => leaveScreen(cue, s)}>Leave {s.name} untouched</button>
      {/if}
    {/each}
  </div>
{/if}

<style>
  .cues {
    grid-area: cues;
  }
  .small {
    padding: 1px 7px;
    font-size: 11px;
  }
  .hint {
    margin-left: auto;
    text-transform: none;
    letter-spacing: 0;
    font-weight: 400;
  }
  .dot {
    display: inline-block;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    margin-left: 3px;
    vertical-align: middle;
  }
  .dot.live {
    background: var(--live);
  }
  .dot.pvw {
    background: var(--pvw);
  }
  .grid {
    flex: 1;
    overflow-y: auto;
    padding: 6px;
    display: grid;
    grid-template-columns: repeat(9, minmax(0, 1fr));
    grid-auto-rows: 84px;
    gap: 5px;
  }
  .slot {
    position: relative;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    justify-content: flex-end;
    text-align: left;
    padding: 4px 6px;
    background: var(--panel-2);
    overflow: hidden;
  }
  .slot.empty {
    background: transparent;
    border-style: dashed;
    color: var(--muted);
  }
  .slot.pvw {
    border: 2px solid var(--pvw);
  }
  .slot.live {
    background: rgba(239, 59, 50, 0.22);
    border: 2px solid var(--live);
  }
  .slot.live.pvw {
    border-color: var(--live);
    box-shadow: inset 0 0 0 2px var(--pvw);
  }
  .num {
    position: absolute;
    top: 3px;
    left: 6px;
    font-size: 10px;
    color: var(--muted);
    background: rgba(0, 0, 0, 0.6);
    padding: 0 3px;
    border-radius: 2px;
  }
  .warn {
    color: var(--warn);
  }
  .mute {
    font-size: 10px;
    background: rgba(0, 0, 0, 0.6);
    border-radius: 2px;
    padding: 0 2px;
  }
  .key {
    position: absolute;
    top: 3px;
    left: 26px;
    font-size: 10px;
    font-weight: 700;
    color: #000;
    background: var(--accent);
    padding: 0 4px;
    border-radius: 2px;
  }
  .thumb {
    flex: 1;
    min-height: 0;
    width: 100%;
    margin-bottom: 2px;
  }
  .edge {
    fill: none;
    stroke: var(--border);
    stroke-width: 1px;
    vector-effect: non-scaling-stroke;
  }
  .defs {
    position: absolute;
  }
  .cname {
    font-weight: 600;
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .meta {
    font-size: 10px;
    color: var(--muted);
  }
  .tags {
    position: absolute;
    top: 3px;
    right: 4px;
    display: flex;
    gap: 2px;
  }
  .air {
    padding: 3px 8px 5px;
    color: #ffb4ae;
    border-bottom: 1px solid var(--border);
    margin-bottom: 3px;
  }
</style>
