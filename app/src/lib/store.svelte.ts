// UI state and the actions the panels call. The controller owns the show: everything that comes
// from it is `$state.raw`, replaced wholesale by its events, never mutated here.
import { ask, open, save } from '@tauri-apps/plugin-dialog';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import * as api from './api';
import type { CueRef, EngineInfo, Live } from './api';
import { cueAt, findLayer, isAutosave, isShowFile, losesPicture, newLayer, onAir, same, sameRef, shiftLayer, withLayer, withScreen, type Scene } from './logic';
import type { Cue } from '$lib/bindings/Cue';
import type { FromEngine } from '$lib/bindings/FromEngine';
import type { Layer } from '$lib/bindings/Layer';
import type { InputInfo } from '$lib/bindings/InputInfo';
import type { MonitorInfo } from '$lib/bindings/MonitorInfo';
import type { OutputState } from '$lib/bindings/OutputState';
import type { Screen } from '$lib/bindings/Screen';
import type { Show } from '$lib/bindings/Show';
import type { ShowOp } from '$lib/bindings/ShowOp';
import type { Source } from '$lib/bindings/Source';
import type { Transition } from '$lib/bindings/Transition';
import type { TransportAction } from '$lib/bindings/TransportAction';

export type LayerRef = { screenId: string; layerId: string };
export type Bus = 'pgm' | 'pvw';

class Store {
  show = $state.raw<Show | null>(null);
  live = $state.raw<Live>({ pvw: null, lastTaken: null, program: {}, preview: {}, transition: { kind: 'Fade', durationMs: 500 } });
  engine = $state.raw<EngineInfo | null>(null);
  /** The operator stopped the engine (outputs black) until they start it. */
  engineStopped = $state(false);
  /** The last "engine restarting: …" message, until the engine is back. */
  restarting = $state('');
  stats = $state.raw<{ fps: number; dropped: number } | null>(null);
  /** Playback position (ms) by layer id, from the engine at 5 Hz. */
  positions = $state.raw<Record<string, number>>({});
  outputs = $state.raw<OutputState[]>([]);
  monitors = $state.raw<MonitorInfo[]>([]);
  /** The engine's last `Inputs` reply (null: not asked yet). */
  inputs = $state.raw<{ list: InputInfo[]; error: string | null } | null>(null);
  error = $state('');
  /** A passing note for the status bar (`toast`). */
  notice = $state('');
  /** The show's file as far as this window knows (null: untitled → Save means Save As). */
  path = $state<string | null>(null);

  activeGroupId = $state<string | null>(null);
  selected = $state<LayerRef | null>(null);
  /** The screen last clicked on the canvas. */
  screenId = $state<string | null>(null);
  /** The bus the canvas, properties and transport work on. */
  canvasTab = $state<Bus>('pvw');
  /** While set, show hotkeys are off. */
  modal = $state<'screens' | 'prompt' | 'menu' | null>(null);
  prompt = $state<{ title: string; value: string; done: (v: string | null) => void } | null>(null);
  /** What is being dragged from Resources, and where the pointer is (a pointer drag: HTML drag
   *  and drop is the native file-drop handler's, which brings files in from the Finder). */
  dragging = $state.raw<{ source: Source; name: string; x: number; y: number } | null>(null);
  /** Files from the file manager are being dragged over the window. */
  fileDrag = $state(false);
  /** Set by the canvas: drops a Resources item at a window point (no-op off the canvas). */
  canvasDrop: ((x: number, y: number, item: { source: Source; name: string }) => void) | null = null;

  group = $derived(this.show?.groups.find((g) => g.id === this.activeGroupId) ?? this.show?.groups[0]);
  /** Where "+ PVW" adds: the selected layer's screen, else the one clicked, else the first. */
  targetScreen = $derived(this.show?.screens.find((s) => s.id === (this.selected?.screenId ?? this.screenId)) ?? this.show?.screens[0]);
  /** What the canvas bus shows, edited directly: Preview's working scene (no cue changes until it
   *  is stored into one), or Program (screens the last Take didn't define keep earlier layers: R41). */
  busScene = $derived(this.canvasTab === 'pvw' ? this.live.preview : this.live.program);
  /** The selected layer as the canvas bus has it. */
  layer = $derived(this.selected ? findLayer(this.busScene, this.selected.screenId, this.selected.layerId) : undefined);
  /** The cue Preview came from, and whether Preview has changed since it was loaded or stored. */
  pvwCue = $derived(cueAt(this.show, this.live.pvw));
  pvwEdited = $derived(!!this.pvwCue && !(same(this.pvwCue.layersByScreen, this.live.preview) && same(this.pvwCue.transition, this.live.transition)));
  /** After a Take PVW still holds what went to air: those clips are Program's (transport them there). */
  pvwLive = $derived(sameRef(this.live.pvw, this.live.lastTaken));
  /** Resizing on the canvas keeps a layer's shape (Shift while dragging does the other). */
  keepAspect = $state(true);
  status = $derived<'connected' | 'held' | 'restarting' | 'connecting' | 'stopped'>(
    this.engineStopped ? 'stopped' : this.engine ? (this.engine.held ? 'held' : 'connected') : this.restarting ? 'restarting' : 'connecting',
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
    case 'Inputs':
      ui.inputs = { list: ev.inputs, error: ev.error };
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
        api.on('engine-stopped', (stopped) => (ui.engineStopped = stopped)),
        getCurrentWebview().onDragDropEvent(({ payload }) => {
          ui.fileDrag = payload.type === 'enter' || payload.type === 'over';
          if (payload.type === 'drop') importDropped(payload.paths);
        }),
        api.on('recovery-available', (p) => offerRecovery(p)),
      ])),
    );
    const [show, live, info, stopped] = await Promise.all([api.getShow(), api.getLive(), api.engineInfo(), api.engineStopped()]);
    ui.engineStopped = stopped;
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

/** Files dropped from the file manager go into the media list (show files are skipped: open those
 *  from File → Open). */
export const importDropped = action(async (paths: string[]) => {
  const media = paths.filter((p) => !isShowFile(p));
  const before = ui.show?.media.length ?? 0;
  if (media.length) {
    await api.importMedia(media);
    await sync();
  }
  const added = (ui.show?.media.length ?? 0) - before;
  const notes = [
    media.length > added && `${media.length - added} already in Media or a folder`,
    paths.length > media.length && `${paths.length - media.length} show file(s) skipped: use Open…`,
  ].filter(Boolean);
  toast([`${added} added to Media`, ...notes].join(' · '));
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
export const listInputs = action(api.listInputs);

/** Stops the engine after asking: every output goes black until Start. */
export const stopEngine = action(async () => {
  if (await confirmAsk('Stop the engine? Every output goes black and stays black until you start it again.', 'Stop engine')) await api.engineStop();
});
export const startEngine = action(api.engineStart);
/** Restarts the engine; asks first while something is on air (outputs blink, clips restart). */
export const restartEngine = action(async () => {
  const air = onAir(ui.live) || ui.engine?.adopted;
  if (air && !(await confirmAsk('Restart the engine? The outputs go black for a moment, then Program comes back with its clips from their first frame.', 'Restart engine'))) return;
  await api.engineRestart();
});

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

/** Add a full-screen layer to Preview (on `screen`, else the selected layer's, else the first). */
export const addToPvw = action(async (source: Source, name: string, screen?: Screen) => {
  screen ??= ui.targetScreen;
  if (!screen) throw new Error('No screens yet: add one in Screen Management');
  const layer = newLayer(source, screen, name);
  const scene = ui.live.preview;
  await api.setPreview(withScreen(scene, screen.id, [...(scene[screen.id] ?? []), layer]), ui.live.transition);
  ui.canvasTab = 'pvw';
  ui.selected = { screenId: screen.id, layerId: layer.id };
});

/** Change (or with null, remove) a layer on the canvas bus: Preview (no cue changes), or Program,
 *  which goes to air at once, so taking picture away (or restarting a clip) asks first. */
export const editLayer = action((sel: LayerRef, f: (l: Layer) => Layer | null) => editBus((sc) => withLayer(sc, sel.screenId, sel.layerId, f)));

/** Move a layer `by` places up (towards the top) or down its screen's stack (±Infinity: to the
 *  top or bottom), as `editLayer` edits. */
export const moveLayer = action((sel: LayerRef, by: number) => editBus((sc) => shiftLayer(sc, sel.screenId, sel.layerId, by)));

async function editBus(f: (sc: Scene) => Scene) {
  const scene = ui.busScene;
  const next = f(scene);
  if (next === scene) return;
  if (ui.canvasTab === 'pvw') return api.setPreview(next, ui.live.transition);
  // An adopted engine never gets SetProgram: the edit reaches air on the next GO, not now.
  if (!ui.engine?.adopted && losesPicture(scene, next)) {
    const yes = await confirmAsk('This changes Program now: a layer disappears, or a clip restarts from its first frame.', 'Change Program');
    if (!yes) return;
  }
  await api.setProgram(next);
}

/** Preview's transition (what GO uses). */
export const setTransition = action((t: Transition) => api.setPreview(ui.live.preview, t));

export const editCue = action(async (ref: CueRef, f: (c: Cue) => Cue) => {
  const cue = cueAt(ui.show, ref);
  if (cue) await setCue(ref, f(cue));
});

/** Click on a slot: load its cue into Preview, or store Preview into the empty slot. */
export const clickSlot = action((ref: CueRef) => (cueAt(ui.show, ref) ? api.loadCue(ref.groupId, ref.slot) : api.storePreview(ref.groupId, ref.slot)));

/** Overwrite the cue at `ref` with Preview (its layers and transition). */
export const updateCue = action((ref: CueRef) => api.storePreview(ref.groupId, ref.slot));

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
