// UI state and the actions the panels call. The controller owns the show: everything that comes
// from it is `$state.raw`, replaced wholesale by its events, never mutated here.
import { ask, open, save } from '@tauri-apps/plugin-dialog';
import * as api from './api';
import type { CueRef, EngineInfo, Live } from './api';
import { copyScene, cueAt, findLayer, firstEmptySlot, forkCue, forksEdit, isAutosave, losesPicture, newCue, newLayer, onAir, sameRef, withLayer, withScreen } from './logic';
import type { Cue } from '$lib/bindings/Cue';
import type { FromEngine } from '$lib/bindings/FromEngine';
import type { Layer } from '$lib/bindings/Layer';
import type { MonitorInfo } from '$lib/bindings/MonitorInfo';
import type { OutputState } from '$lib/bindings/OutputState';
import type { Screen } from '$lib/bindings/Screen';
import type { Show } from '$lib/bindings/Show';
import type { ShowOp } from '$lib/bindings/ShowOp';
import type { Source } from '$lib/bindings/Source';
import type { TransportAction } from '$lib/bindings/TransportAction';

export type LayerRef = { screenId: string; layerId: string };
export type Bus = 'pgm' | 'pvw';

class Store {
  show = $state.raw<Show | null>(null);
  live = $state.raw<Live>({ pvw: null, lastTaken: null, program: {} });
  engine = $state.raw<EngineInfo | null>(null);
  /** The last "engine restarting: …" message, until the engine is back. */
  restarting = $state('');
  stats = $state.raw<{ fps: number; dropped: number } | null>(null);
  /** Playback position (ms) by layer id, from the engine at 5 Hz. */
  positions = $state.raw<Record<string, number>>({});
  outputs = $state.raw<OutputState[]>([]);
  monitors = $state.raw<MonitorInfo[]>([]);
  error = $state('');
  /** A passing note for the status bar (`toast`). */
  notice = $state('');
  /** The show's file as far as this window knows (null: untitled → Save means Save As). */
  path = $state<string | null>(null);

  activeGroupId = $state<string | null>(null);
  selected = $state<LayerRef | null>(null);
  /** The bus the canvas, properties and transport work on. */
  canvasTab = $state<Bus>('pvw');
  /** While set, show hotkeys are off. */
  modal = $state<'screens' | 'prompt' | 'menu' | null>(null);
  prompt = $state<{ title: string; value: string; done: (v: string | null) => void } | null>(null);
  /** What is being dragged from Resources (WebKit's dataTransfer is unreliable for custom types). */
  dragging = $state.raw<{ source: Source; name: string } | null>(null);

  group = $derived(this.show?.groups.find((g) => g.id === this.activeGroupId) ?? this.show?.groups[0]);
  /** The cue on the canvas bus: PVW's cue, or the last taken one for PGM. */
  editRef = $derived(this.canvasTab === 'pvw' ? this.live.pvw : this.live.lastTaken);
  editCue = $derived(cueAt(this.show, this.editRef));
  /** The selected layer if it is in `editCue` (editable). */
  layer = $derived(this.selected && this.editCue ? findLayer(this.editCue.layersByScreen, this.selected.screenId, this.selected.layerId) : undefined);
  /** What the canvas bus shows. On PGM that is more than the last taken cue: screens it doesn't
   *  define keep earlier cues' layers, which transport must reach too (R41). */
  busScene = $derived(this.canvasTab === 'pvw' ? (this.editCue?.layersByScreen ?? {}) : this.live.program);
  /** The selected layer as the bus has it (transport's target). */
  busLayer = $derived(this.selected ? findLayer(this.busScene, this.selected.screenId, this.selected.layerId) : undefined);
  /** After a Take the same cue stays in PVW. Edits from the Preview tab then go to a copy (R44). */
  pvwLive = $derived(sameRef(this.live.pvw, this.live.lastTaken));
  status = $derived<'connected' | 'held' | 'restarting' | 'connecting'>(
    this.engine ? (this.engine.held ? 'held' : 'connected') : this.restarting ? 'restarting' : 'connecting',
  );
}

export const ui = new Store();

export function fail(e: unknown) {
  ui.error = e instanceof Error ? e.message : String(e);
  console.error(ui.error);
}

let noticeTimer: ReturnType<typeof setTimeout> | undefined;
/** Shows `message` in the status bar for a few seconds. */
export function toast(message: string) {
  ui.notice = message;
  clearTimeout(noticeTimer);
  noticeTimer = setTimeout(() => (ui.notice = ''), 5000);
}

/** Read back what the controller holds. Its events say the same, but may arrive after a
 *  command's reply, and the next edit must build on this one. */
async function sync() {
  [ui.show, ui.live] = await Promise.all([api.getShow(), api.getLive()]);
}

let queue: Promise<void> = Promise.resolve();

/** Actions run one at a time, each followed by a `sync`, so a second quick edit never rebuilds a
 *  cue from the show as it was before the first. Errors (a command's `Err`, a dialog failure) go
 *  to the status bar and the UI carries on. Never call an action from inside another (it would
 *  wait for itself). */
function action<A extends unknown[]>(f: (...a: A) => Promise<unknown>) {
  return (...a: A): Promise<void> => (queue = queue.then(() => f(...a)).catch(fail).then(sync).catch(fail));
}

function engineEvent(ev: FromEngine) {
  switch (ev.type) {
    case 'Stats':
      ui.stats = { fps: ev.fps, dropped: ev.dropped };
      break;
    case 'Positions':
      ui.positions = ev.positions;
      break;
    case 'OutputStatus':
      ui.outputs = ev.outputs;
      break;
    case 'Monitors':
      ui.monitors = ev.monitors;
      break;
    case 'Probed':
      if (ev.error) fail(`probe: ${ev.error}`);
      break;
    case 'Error':
      if (ev.message.startsWith('engine restarting')) ui.restarting = ev.message;
      else fail(ev.message);
      break;
  }
}

function setEngine(info: EngineInfo | null) {
  ui.engine = info;
  if (info) ui.restarting = '';
  else ui.stats = null;
}

/** Startup. Returns the cleanup (unlisten). */
export async function init(): Promise<() => void> {
  // A bug in a panel shows in the status bar instead of failing silently.
  const onError = (e: ErrorEvent) => fail(e.message);
  const onRejection = (e: PromiseRejectionEvent) => fail(e.reason);
  const onCsp = (e: SecurityPolicyViolationEvent) => fail(`blocked by the content security policy: ${e.blockedURI} (${e.violatedDirective})`);
  addEventListener('error', onError);
  addEventListener('unhandledrejection', onRejection);
  addEventListener('securitypolicyviolation', onCsp);
  const offs = [
    () => removeEventListener('error', onError),
    () => removeEventListener('unhandledrejection', onRejection),
    () => removeEventListener('securitypolicyviolation', onCsp),
  ];
  try {
    // First: open_show resets the recovery offer.
    const recovery = await api.getRecovery();
    offs.push(
      ...(await Promise.all([
        api.on('show-changed', (s) => (ui.show = s)),
        api.on('live-changed', (l) => (ui.live = l)),
        api.on('engine-event', engineEvent),
        api.on('engine-info', setEngine),
        api.on('recovery-available', (p) => offerRecovery(p)),
      ])),
    );
    const [show, live, info] = await Promise.all([api.getShow(), api.getLive(), api.engineInfo()]);
    ui.show = show;
    ui.live = live;
    setEngine(info);
    if (recovery) await offerRecovery(recovery);
  } catch (e) {
    fail(e);
  }
  return () => offs.forEach((f) => f());
}

const offerRecovery = action(async (path: string) => {
  const yes = await ask(`Unsaved work from an earlier session was kept:\n${path}\n\nRecover it now? (It opens as an untitled show.)`, {
    title: 'Recover unsaved work?', kind: 'warning', okLabel: 'Recover', cancelLabel: 'Not now',
  });
  if (yes) await openShowAt(path);
});

const showFilters = [{ name: 'Tessera show', extensions: ['tshow'] }];
/** Also lets a kept autosave be opened by hand. */
const openFilters = [{ name: 'Tessera show or autosave', extensions: ['tshow', 'autosave', 'prev'] }];

/** Opening keeps PGM running, but the outputs switch to the opened show's setup (I6). */
async function openShowAt(path: string) {
  if (onAir(ui.live)) {
    const name = path.split(/[\\/]/).pop();
    const yes = await ask(`Open ${name}? What is on air keeps playing, but outputs, screens and mappings switch to that show's setup now, and unsaved changes here are lost.`, {
      title: 'Open while on air', kind: 'warning', okLabel: 'Open',
    });
    if (!yes) return;
  }
  await api.openShow(path);
  ui.path = isAutosave(path) ? null : path;
  ui.selected = null;
}

export const newShow = action(async () => {
  const yes = await ask('Start a new, empty show? Outputs close until screens are set up again, and unsaved changes are lost.', {
    title: 'New show', kind: 'warning', okLabel: 'New show',
  });
  if (!yes) return;
  await api.newShow();
  ui.path = null;
  ui.selected = null;
});

export const openShow = action(async () => {
  const path = await open({ filters: openFilters });
  if (path) await openShowAt(path);
});

export const saveShow = action(async (as = false) => {
  if (ui.path && !as) return api.saveShow(null);
  const path = await save({ defaultPath: ui.path ?? 'Untitled.tshow', filters: showFilters });
  if (!path) return;
  await api.saveShow(path);
  ui.path = path;
});

export const importMedia = action(async () => {
  // No filter: FFmpeg plays more than any list here; the probe reports what it can't.
  const paths = await open({ multiple: true });
  if (paths?.length) await api.importMedia(paths);
});

export const undo = action(api.undo);
export const redo = action(api.redo);
export const take = action(api.take);
export const loadNext = action(api.loadNext);
export const op = action((o: ShowOp) => api.applyOp(o));

/** Send `ops` one by one (Screen Management's Apply). The first failure stops the rest. Resolves
 *  to '' when all were applied, else the error, which says how many went through. */
export function applyOps(ops: ShowOp[]): Promise<string> {
  let error = '';
  return action(async () => {
    for (const [i, o] of ops.entries()) {
      try {
        await api.applyOp(o);
      } catch (e) {
        error = `${i} of ${ops.length} changes applied, then ${o.op} failed: ${e instanceof Error ? e.message : e}`;
        throw new Error(error);
      }
    }
  })().then(() => error);
}
const setCue = (ref: CueRef, cue: Cue) => api.applyOp({ op: 'SetCue', groupId: ref.groupId, cue });

/** Store `next`, an edit of the PVW cue at `ref`. While that cue is on air (R44) the edit goes to a
 *  copy in its group's first empty slot instead, which is loaded into PVW; the on-air cue and
 *  Program stay as they are. Returns the copy's layer ids (old → new), or null if not copied. */
async function setPvwCue(ref: CueRef, next: Cue): Promise<Record<string, string> | null> {
  if (!forksEdit('pvw', ui.live)) {
    await setCue(ref, next);
    return null;
  }
  const group = ui.show?.groups.find((g) => g.id === ref.groupId);
  if (!group) throw new Error('The on-air cue’s group is gone: nothing changed');
  const { cue, ids } = forkCue(group, next);
  await setCue({ groupId: group.id, slot: cue.slot }, cue);
  await api.loadCue(group.id, cue.slot);
  toast(`Copied to slot ${cue.slot + 1} — on-air cue unchanged`);
  return ids;
}

/** Add a full-screen layer to the PVW cue (on `screen`, else the selected layer's, else the first).
 *  With nothing in PVW, a new cue in the active group's first free slot is created and loaded. */
export const addToPvw = action(async (source: Source, name: string, screen?: Screen) => {
  const show = ui.show;
  if (!show) return;
  screen ??= show.screens.find((s) => s.id === ui.selected?.screenId) ?? show.screens[0];
  if (!screen) throw new Error('No screens yet: add one in Screen Management');
  const layer = newLayer(source, screen, name);
  const add = (c: Cue) => withScreen(c, screen.id, [...(c.layersByScreen[screen.id] ?? []), layer]);
  const ref = ui.live.pvw;
  const cue = cueAt(show, ref);
  let ids: Record<string, string> | null = null;
  if (cue && ref) {
    ids = await setPvwCue(ref, add(cue));
  } else {
    const group = ui.group;
    if (!group) return;
    const slot = firstEmptySlot(group);
    await setCue({ groupId: group.id, slot }, add(newCue(slot)));
    await api.loadCue(group.id, slot);
  }
  ui.canvasTab = 'pvw';
  ui.selected = { screenId: screen.id, layerId: ids?.[layer.id] ?? layer.id };
});

/** Change (or with null, remove) a layer of the cue on the canvas bus. From the Preview tab an edit
 *  to the on-air cue goes to a copy (R44). From the Program tab it changes Program at once, so
 *  taking picture away (or restarting a clip) asks first. */
export const editLayer = action(async (sel: LayerRef, f: (l: Layer) => Layer | null) => {
  const ref = ui.editRef;
  const cue = ui.editCue;
  const next = cue && withLayer(cue, sel.screenId, sel.layerId, f);
  if (!ref || !cue || !next || next === cue) return;
  if (ui.canvasTab === 'pvw') {
    const ids = await setPvwCue(ref, next);
    const s = ui.selected;
    if (ids && s) ui.selected = { screenId: s.screenId, layerId: ids[s.layerId] ?? s.layerId };
    return;
  }
  // An adopted engine never gets SetProgram: the edit reaches air on the next GO, not now.
  if (sameRef(ref, ui.live.lastTaken) && !ui.engine?.adopted && losesPicture(cue, next)) {
    const name = findLayer(cue.layersByScreen, sel.screenId, sel.layerId)?.name || 'this layer';
    const yes = await confirmAsk(`Cue ${cue.slot + 1} “${cue.name}” is on air. This changes “${name}” on Program now: it disappears, or restarts from its first frame.`, 'Change Program');
    if (!yes) return;
  }
  await setCue(ref, next);
});

export const editCue = action(async (ref: CueRef, f: (c: Cue) => Cue) => {
  const cue = cueAt(ui.show, ref);
  if (cue) await setCue(ref, f(cue));
});

/** Click on a slot: load its cue, or create one there from the PVW cue's layers (or empty) and load it. */
export const clickSlot = action(async (ref: CueRef) => {
  if (!cueAt(ui.show, ref)) {
    const pvw = cueAt(ui.show, ui.live.pvw);
    await setCue(ref, newCue(ref.slot, pvw ? copyScene(pvw.layersByScreen) : {}, pvw?.transition));
  }
  await api.loadCue(ref.groupId, ref.slot);
});

export const fire = action(async (ref: CueRef) => {
  await api.loadCue(ref.groupId, ref.slot);
  await api.take();
});

export const transport = action(async (bus: Bus, layer: string | null, act: TransportAction) => {
  if (bus === 'pgm' && (act === 'Stop' || act === 'Restart')) {
    const what = layer ? 'this layer' : 'every layer';
    const yes = await ask(`${act} ${what} on PROGRAM? It is on air: the picture blanks until the clip shows its first frame again.`, {
      title: `${act} on air`, kind: 'warning', okLabel: act,
    });
    if (!yes) return;
  }
  await api.transport(bus === 'pgm' ? 'Pgm' : 'Pvw', layer, act);
});

/** A text prompt in the window (the dialog plugin has none). Resolves null on cancel. */
export function promptText(title: string, value = ''): Promise<string | null> {
  return new Promise((done) => {
    ui.prompt = { title, value, done };
    ui.modal = 'prompt';
  });
}

export const confirmAsk = (message: string, okLabel: string) => ask(message, { title: 'Tessera', kind: 'warning', okLabel });

export const addGroup = action(async (name: string) => {
  await api.applyOp({ op: 'AddGroup', name });
  await sync();
  ui.activeGroupId = ui.show?.groups.at(-1)?.id ?? null;
});
