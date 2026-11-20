// Pure helpers for the UI: no runes, no Tauri, so `npm test` runs them in plain Node.
import type { Cue } from './bindings/Cue';
import type { Group } from './bindings/Group';
import type { Layer } from './bindings/Layer';
import type { Mapping } from './bindings/Mapping';
import type { MediaItem } from './bindings/MediaItem';
import type { MonitorInfo } from './bindings/MonitorInfo';
import type { Output } from './bindings/Output';
import type { Rect } from './bindings/Rect';
import type { Screen } from './bindings/Screen';
import type { Show } from './bindings/Show';
import type { ShowOp } from './bindings/ShowOp';
import type { Source } from './bindings/Source';
import type { Transition } from './bindings/Transition';
import type { CueRef, EngineInfo, Live } from './api';

export type Scene = Cue['layersByScreen'];
export type Bounds = { x: number; y: number; w: number; h: number };
export type DragMode = 'move' | 'resize';

/** The preview's extent: every screen at its layout position (the engine's `render_layout`). */
export function layoutBounds(screens: Screen[]): Bounds {
  if (!screens.length) return { x: 0, y: 0, w: 1, h: 1 };
  const x = Math.min(...screens.map((s) => s.layoutX));
  const y = Math.min(...screens.map((s) => s.layoutY));
  const r = Math.max(...screens.map((s) => s.layoutX + s.width));
  const b = Math.max(...screens.map((s) => s.layoutY + s.height));
  return { x, y, w: Math.max(r - x, 1), h: Math.max(b - y, 1) };
}

/** Display pixels per layout pixel for `b` fitted into `width` x `height`. */
export function fitScale(b: Bounds, width: number, height: number): number {
  return Math.max(0, Math.min(width / b.w, height / b.h));
}

/** The screen under a layout point; the last one wins, as it is drawn on top. */
export function screenAt(screens: Screen[], x: number, y: number): Screen | undefined {
  return screens.findLast((s) => x >= s.layoutX && x < s.layoutX + s.width && y >= s.layoutY && y < s.layoutY + s.height);
}

/** A layer rect (screen pixels) placed in display pixels on the preview. */
export function boxOf(rect: Rect, screen: Screen, b: Bounds, scale: number): Rect {
  return { x: (screen.layoutX - b.x + rect.x) * scale, y: (screen.layoutY - b.y + rect.y) * scale, w: rect.w * scale, h: rect.h * scale };
}

/** `r` after a drag by (dx, dy) layout pixels: moved, or resized from its bottom-right corner. */
export function dragRect(r: Rect, dx: number, dy: number, mode: DragMode): Rect {
  return mode === 'move'
    ? { ...r, x: Math.round(r.x + dx), y: Math.round(r.y + dy) }
    : { ...r, w: Math.max(1, Math.round(r.w + dx)), h: Math.max(1, Math.round(r.h + dy)) };
}

export const sameRef = (a: CueRef | null, b: CueRef | null) => !!a && !!b && a.groupId === b.groupId && a.slot === b.slot;

export function cueAt(show: Show | null, ref: CueRef | null): Cue | undefined {
  if (!show || !ref) return undefined;
  return show.groups.find((g) => g.id === ref.groupId)?.cues.find((c) => c.slot === ref.slot);
}

/** The lowest free slot after `after`. */
export function firstEmptySlot(group: Group, after = -1): number {
  const used = new Set(group.cues.map((c) => c.slot));
  let s = after + 1;
  while (used.has(s)) s++;
  return s;
}

/** Slots the grid shows: at least 27, whole rows of 9, and always a free row after the last cue. */
export function slotCount(group: Group): number {
  const last = Math.max(-1, ...group.cues.map((c) => c.slot));
  return Math.max(27, Math.ceil((last + 2) / 9) * 9);
}

/** A random v4 UUID. `crypto.randomUUID` needs a secure context; this works in any webview. */
export function uuid(): string {
  const b = crypto.getRandomValues(new Uint8Array(16));
  b[6] = (b[6] & 0x0f) | 0x40;
  b[8] = (b[8] & 0x3f) | 0x80;
  const h = Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('');
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
}

/** A full-screen layer with neutral effects (the Rust `Layer::new`). */
export function newLayer(source: Source, screen: Screen, name: string): Layer {
  return {
    id: uuid(), name, source, rect: { x: 0, y: 0, w: screen.width, h: screen.height },
    opacity: 1, brightness: 0, contrast: 1, saturation: 1, volume: 1, playback: 'Loop', visible: true,
  };
}

export function newCue(slot: number, layersByScreen: Scene = {}, transition: Transition = { kind: 'Fade', durationMs: 500 }): Cue {
  return { id: uuid(), name: `Cue ${slot + 1}`, slot, layersByScreen, transition: { ...transition } };
}

/** A copy of `scene` with fresh layer ids (as `DuplicateCue` does), so no two cues share a layer. */
export function copyScene(scene: Scene): Scene {
  return Object.fromEntries(Object.entries(scene).map(([s, ls]) => [s, ls.map((l) => ({ ...l, id: uuid() }))]));
}

/** `cue` with `layers` on screen `screenId` (`null` drops the screen: the cue no longer touches it). */
export function withScreen(cue: Cue, screenId: string, layers: Layer[] | null): Cue {
  const { [screenId]: _, ...rest } = cue.layersByScreen;
  return { ...cue, layersByScreen: layers ? { ...rest, [screenId]: layers } : rest };
}

export const findLayer = (scene: Scene, screenId: string, layerId: string) => scene[screenId]?.find((l) => l.id === layerId);

/** `cue` with one layer replaced by `f(layer)`, or removed when `f` returns null. The same `cue`
 *  when it has no such layer: an edit aimed elsewhere must never black out one of its screens. */
export function withLayer(cue: Cue, screenId: string, layerId: string, f: (l: Layer) => Layer | null): Cue {
  const layers = cue.layersByScreen[screenId];
  if (!layers?.some((l) => l.id === layerId)) return cue;
  return withScreen(cue, screenId, layers.flatMap((l) => (l.id === layerId ? (f(l) ?? []) : [l])));
}

/** The media item a layer plays, if it plays one. */
export function mediaOf(show: Show | null, layer: Layer | undefined): MediaItem | undefined {
  const src = layer?.source;
  return src?.type === 'Media' ? show?.media.find((m) => m.id === src.mediaId) : undefined;
}

/** Whether `after` shows less than `before`: a layer that was showing is removed, hidden or at
 *  opacity 0, or gets another source or playback mode where a clip is involved (the engine opens a
 *  new player, which blanks until its first frame and starts from the top: M6), or a screen the
 *  cue didn't define becomes explicit black. On the cue that is on air such an edit changes
 *  Program at once (SetProgram), so the UI asks first. A screen dropped from the cue
 *  (`withScreen(…, null)`) loses nothing: Program keeps what it shows there. */
export function losesPicture(before: Cue, after: Cue): boolean {
  const shown = (c: Cue, s: string) => (c.layersByScreen[s] ?? []).filter((l) => l.visible && l.opacity > 0);
  const media = (l: Layer) => l.source.type === 'Media';
  const newPlayer = (a: Layer, b: Layer) => (media(a) || media(b)) && (!same(a.source, b.source) || a.playback !== b.playback);
  return Object.entries(after.layersByScreen).some(([s, layers]) => {
    const now = new Map(shown(after, s).map((l) => [l.id, l]));
    const lost = (l: Layer) => {
      const n = now.get(l.id);
      return !n || newPlayer(l, n);
    };
    return shown(before, s).some(lost) || (!layers.length && !(s in before.layersByScreen));
  });
}

/** R44: an edit made from the Preview tab while PVW holds the cue that is on air never goes to air:
 *  it goes to a copy of the cue (`forkCue`) instead. */
export const forksEdit = (tab: 'pvw' | 'pgm', live: Live) => tab === 'pvw' && sameRef(live.pvw, live.lastTaken);

/** A copy of `cue` for `group`'s first empty slot: fresh cue and layer ids, the same layers on the
 *  same screens and the same transition, so PVW still shares PGM's running players (the engine
 *  matches screen, media and position) and the next Take continues them. `ids`: old → new layer id. */
export function forkCue(group: Group, cue: Cue): { cue: Cue; ids: Record<string, string> } {
  const ids: Record<string, string> = {};
  const layersByScreen = Object.fromEntries(Object.entries(cue.layersByScreen).map(([s, ls]) => [s, ls.map((l) => ({ ...l, id: (ids[l.id] = uuid()) }))]));
  return { cue: { ...cue, id: uuid(), slot: firstEmptySlot(group), layersByScreen, transition: { ...cue.transition } }, ids };
}

/** An autosave, or one kept from an earlier session (`….autosave.prev`, `….autosave.<ms>.prev`):
 *  it opens as untitled work. */
export const isAutosave = (p: string) => /\.autosave(\.\d+(-\d+)?)?\.prev$|\.autosave$/.test(p);

/** `http://127.0.0.1:<port><path>?token=…`, or null while the engine is disconnected. */
export function engineUrl(e: EngineInfo | null, path: string): string | null {
  return e ? `http://127.0.0.1:${e.port}${path}?token=${encodeURIComponent(e.token)}` : null;
}

export type Hotkey = 'take' | 'next' | 'undo' | 'redo' | 'save' | 'saveAs';
type KeyLike = { key: string; code?: string; metaKey: boolean; ctrlKey: boolean; shiftKey: boolean; altKey: boolean };

export function hotkey(e: KeyLike): Hotkey | null {
  const mod = e.metaKey || e.ctrlKey;
  if (e.altKey) return null;
  if (!mod) {
    if (e.shiftKey) return null;
    if (e.code === 'Space' || e.key === ' ') return 'take';
    return e.key === 'ArrowRight' ? 'next' : null;
  }
  const k = e.key.toLowerCase();
  if (k === 'z') return e.shiftKey ? 'redo' : 'undo';
  if (k === 's') return e.shiftKey ? 'saveAs' : 'save';
  return null;
}

type Focusable = { tagName?: string; isContentEditable?: boolean; getAttribute?: (name: string) => string | null };

/** Focus on any interactive element keeps the keys (R40): Space on a focused button or checkbox
 *  would work the control *and* GO on air. Nothing focused (body) gives them to the show; the
 *  page blurs controls after use so that is the usual state. */
export function ownsKeys(el: Focusable | null): boolean {
  if (!el) return false;
  if (['BUTTON', 'INPUT', 'SELECT', 'TEXTAREA'].includes(el.tagName ?? '') || el.isContentEditable) return true;
  return ['button', 'slider'].includes(el.getAttribute?.('role') ?? '');
}

/** "#ff8000" → opaque [1, 0.5, 0, 1] (a `Solid` pattern's rgba), and back. */
export const hexToRgba = (hex: string): [number, number, number, number] => {
  const [r, g, b] = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255);
  return [r, g, b, 1];
};
export const rgbaToHex = (c: number[]) =>
  '#' + c.slice(0, 3).map((v) => Math.round(Math.min(1, Math.max(0, v)) * 255).toString(16).padStart(2, '0')).join('');

/** 83500 → "1:23.5" */
export function fmtTime(ms: number): string {
  const t = Math.round(Math.max(0, ms) / 100); // tenths
  return `${Math.floor(t / 600)}:${((t % 600) / 10).toFixed(1).padStart(4, '0')}`;
}

// ---- Screen Management (T13) ----

/** What Screen Management edits: a local copy, sent as ops on Apply. */
export type Setup = Pick<Show, 'outputs' | 'screens' | 'mappings'>;

/** Largest magnitude a numeric field takes (keeps f32/i32 fields far from overflow). */
export const MAX_NUM = 1_000_000;

/** A numeric field's text → the number, or the message to show when it is not finite, larger
 *  than ±MAX_NUM, below `min`, or (with `int`) not whole. */
export function parseNum(text: string, { int = false, min = -MAX_NUM } = {}): number | string {
  const v = text.trim() ? Number(text) : NaN;
  if (Number.isFinite(v) && v >= min && Math.abs(v) <= MAX_NUM && (!int || Number.isInteger(v))) return v;
  return `Enter ${int ? 'a whole number' : 'a number'} from ${min} to ${MAX_NUM}`;
}

/** Deep equality of JSON-like values; object key order doesn't matter. */
export function same(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (!a || !b || typeof a !== 'object' || typeof b !== 'object' || Array.isArray(a) !== Array.isArray(b)) return false;
  const [x, y] = [a as Record<string, unknown>, b as Record<string, unknown>];
  const keys = Object.keys(x);
  return keys.length === Object.keys(y).length && keys.every((k) => k in y && same(x[k], y[k]));
}

/** A cue has been taken, or Program shows something: output changes are seen on air. */
export const onAir = (live: Live) => !!live.lastTaken || Object.keys(live.program).length > 0;

const output = (name: string, target: Output['target'], width: number, height: number): Output =>
  ({ id: uuid(), name, node: 'local', target, width, height, brightness: 1 });

/** Fullscreen on `m`, at its size; the engine binds it by name (then position). */
export const displayOutput = (m: MonitorInfo, name = m.name): Output =>
  output(name, { type: 'Display', monitorName: m.name, monitorX: m.x, monitorY: m.y }, m.width, m.height);

export const windowOutput = (name: string, x: number, y: number, width: number, height: number): Output =>
  output(name, { type: 'Window', x, y }, width, height);

/** A 1920×1080 screen placed right of the current layout. */
export function newScreen(screens: Screen[]): Screen {
  const b = layoutBounds(screens);
  return { id: uuid(), name: `Screen ${screens.length + 1}`, width: 1920, height: 1080, layoutX: screens.length ? b.x + b.w : 0, layoutY: screens.length ? b.y : 0 };
}

/** The whole screen onto the whole output. */
export const newMapping = (s: Screen, o: Output): Mapping =>
  ({ id: uuid(), screenId: s.id, src: { x: 0, y: 0, w: s.width, h: s.height }, outputId: o.id, dst: { x: 0, y: 0, w: o.width, h: o.height } });

/** The ops that turn `before` (the show) into `after` (the edited copy), only for what changed.
 *  Mapping removes come before output/screen removes; outputs and screens exist before the
 *  mappings that use them. Removing a screen also drops its layers from every cue (show cascade). */
export function setupOps(before: Setup, after: Setup): ShowOp[] {
  const diff = <T extends { id: string }>(a: T[], b: T[]) => ({
    gone: a.filter((x) => !b.some((y) => y.id === x.id)),
    added: b.filter((y) => !a.some((x) => x.id === y.id)),
    changed: b.filter((y) => a.some((x) => x.id === y.id && !same(x, y))),
  });
  const [o, s, m] = [diff(before.outputs, after.outputs), diff(before.screens, after.screens), diff(before.mappings, after.mappings)];
  return [
    ...m.gone.map((x): ShowOp => ({ op: 'RemoveMapping', id: x.id })),
    ...o.gone.map((x): ShowOp => ({ op: 'RemoveOutput', id: x.id })),
    ...s.gone.map((x): ShowOp => ({ op: 'RemoveScreen', id: x.id })),
    ...o.added.map((output): ShowOp => ({ op: 'AddOutput', output })),
    ...o.changed.map((output): ShowOp => ({ op: 'UpdateOutput', output })),
    ...s.added.map((screen): ShowOp => ({ op: 'AddScreen', screen })),
    ...s.changed.map((screen): ShowOp => ({ op: 'UpdateScreen', screen })),
    ...[...m.added, ...m.changed].map((mapping): ShowOp => ({ op: 'SetMapping', mapping })),
  ];
}
