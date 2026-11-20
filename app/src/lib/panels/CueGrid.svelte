<script lang="ts">
  import type { Cue } from '$lib/bindings/Cue';
  import { firstEmptySlot, sameRef, slotCount, withScreen } from '$lib/logic';
  import { addGroup, clickSlot, confirmAsk, editCue, fire, op, promptText, ui } from '$lib/store.svelte';
  import type { CueRef } from '$lib/api';

  const group = $derived(ui.group);
  const slots = $derived.by(() => {
    if (!group) return [];
    const bySlot = new Map(group.cues.map((c) => [c.slot, c]));
    return Array.from({ length: slotCount(group) }, (_, slot) => ({ slot, cue: bySlot.get(slot) }));
  });
  const ref = (slot: number): CueRef => ({ groupId: group?.id ?? '', slot });

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

  // A double-click on an empty slot creates the cue with its first click: it must not also go to air.
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
  const clearScreen = (cue: Cue, s: { id: string; name: string }) =>
    onAir(cue, `${s.name} goes black on Program now.`, 'Clear on air', () => editCue(ref(cue.slot), (c) => withScreen(c, s.id, [])));
  const leaveScreen = (cue: Cue, s: { id: string; name: string }) =>
    onAir(cue, `Program keeps ${s.name} as it is now; from the next GO this cue leaves ${s.name} untouched.`, 'Leave untouched', () =>
      editCue(ref(cue.slot), (c) => withScreen(c, s.id, null)),
    );

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
    if (menu && e.key === 'Escape') closeMenu();
  }}
/>

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
    <span class="hint">click: load to Preview · double-click: load + GO · right-click: more</span>
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
        title={cue ? cue.name : 'Empty: click to create a cue from Preview'}
      >
        <span class="num">{slot + 1}</span>
        {#if cue}
          <span class="cname">{cue.name}</span>
          <span class="meta">{cue.transition.kind}{cue.transition.kind !== 'Cut' ? ` ${cue.transition.durationMs}` : ''} · {Object.keys(cue.layersByScreen).length} scr</span>
        {/if}
        <span class="tags">
          {#if live}<span class="badge live">PGM</span>{/if}
          {#if pvw}<span class="badge pvw">PVW</span>{/if}
        </span>
      </button>
    {/each}
  </div>
</section>

{#if menu}
  {@const cue = menu.cue}
  <div class="menu" class:up={menu.y > innerHeight / 2} class:left={menu.x > innerWidth - 240} style:left="{menu.x}px" style:top="{menu.y}px">
    {#if sameRef(ui.live.lastTaken, ref(cue.slot))}<div class="air"><span class="badge live">ON AIR</span> cue {cue.slot + 1} is on Program</div>{/if}
    <button onclick={() => renameCue(cue)}>Rename…</button>
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
    grid-auto-rows: 58px;
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
  .menu {
    position: fixed;
    z-index: 40;
    min-width: 200px;
    background: var(--panel-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 4px;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.5);
    display: flex;
    flex-direction: column;
  }
  .menu.up {
    transform: translateY(-100%);
  }
  .menu.left {
    translate: -100% 0;
  }
  .air {
    padding: 3px 8px 5px;
    color: #ffb4ae;
    border-bottom: 1px solid var(--border);
    margin-bottom: 3px;
  }
  .menu button {
    background: none;
    border: none;
    text-align: left;
    padding: 4px 8px;
  }
  .menu button:hover {
    background: var(--accent-bg);
    color: var(--accent-2);
  }
  .menu hr {
    border: none;
    border-top: 1px solid var(--border);
    margin: 3px 0;
    width: 100%;
  }
</style>
