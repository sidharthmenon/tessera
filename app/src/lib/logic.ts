// Pure helpers for the UI: no runes, no Tauri, so `npm test` runs them in plain Node.
import type { Cue } from './bindings/Cue';
import type { Group } from './bindings/Group';
import type { Layer } from './bindings/Layer';
import type { Mapping } from './bindings/Mapping';
import type { MediaItem } from './bindings/MediaItem';
import type { MonitorInfo } from './bindings/MonitorInfo';
import type { Output } from './bindings/Output';
import type { Pattern } from './bindings/Pattern';
import type { Rect } from './bindings/Rect';
import type { Screen } from './bindings/Screen';
import type { Show } from './bindings/Show';
import type { ShowOp } from './bindings/ShowOp';
import type { Source } from './bindings/Source';
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

/** `r` with its dragged edges pulled onto the nearest line within `tol`: x edges onto `xs`, y edges
 *  onto `ys`. Moving pulls whichever edge is closer; resizing (bottom-right corner) its right and
 *  bottom edges. */
export function snapRect(r: Rect, mode: DragMode, xs: number[], ys: number[], tol: number): Rect {
  const pull = (edges: number[], lines: number[]) => {
    let d = Infinity;
    for (const e of edges) for (const l of lines) if (Math.abs(l - e) < Math.abs(d)) d = l - e;
    return Math.abs(d) <= tol ? d : 0;
  };
  return mode === 'move'
    ? { ...r, x: r.x + pull([r.x, r.x + r.w], xs), y: r.y + pull([r.y, r.y + r.h], ys) }
    : { ...r, w: Math.max(1, r.w + pull([r.x + r.w], xs)), h: Math.max(1, r.h + pull([r.y + r.h], ys)) };
}

/** `r` at aspect `a` (w/h), its top-left kept: sized by its width, its height, or by default
 *  whichever makes it larger. */
export function lockAspect(r: Rect, a: number, by: 'w' | 'h' = r.w / r.h > a ? 'w' : 'h'): Rect {
  return by === 'w' ? { ...r, h: Math.max(1, Math.round(r.w / a)) } : { ...r, w: Math.max(1, Math.round(r.h * a)) };
}

/** The largest rect at aspect `a` inside a `w`×`h` frame, centred. */
export function fitInto(a: number, w: number, h: number): Rect {
  if (w / h > a) {
    const fw = Math.round(h * a);
    return { x: Math.round((w - fw) / 2), y: 0, w: fw, h };
  }
  const fh = Math.round(w / a);
  return { x: 0, y: Math.round((h - fh) / 2), w, h: fh };
}

/** A drag's rect snapped (`snapToEdges`) and, when resizing with `aspect` (w/h) given, kept at that
 *  shape: the shaped rect snaps, then is re-shaped from whichever side snapped. */
export function snapDrag(r: Rect, mode: DragMode, others: Rect[], w: number, h: number, scale: number, aspect?: number): Rect {
  if (!aspect || mode === 'move') return snapToEdges(r, mode, others, w, h, scale);
  const locked = lockAspect(r, aspect);
  const snapped = snapToEdges(locked, mode, others, w, h, scale);
  return lockAspect(snapped, aspect, snapped.w === locked.w && snapped.h !== locked.h ? 'h' : 'w');
}

/** Within this many display pixels, a dragged edge snaps. */
const SNAP_PX = 8;

/** `r`, dragged on an editor drawn at `scale` display pixels per unit, snapped to the frame's edges
 *  (0..`w`, 0..`h`: a screen, an output) and to `others`' edges. */
export function snapToEdges(r: Rect, mode: DragMode, others: Rect[], w: number, h: number, scale: number): Rect {
  const xs = [0, w, ...others.flatMap((o) => [o.x, o.x + o.w])];
  const ys = [0, h, ...others.flatMap((o) => [o.y, o.y + o.h])];
  return snapRect(r, mode, xs, ys, SNAP_PX / scale);
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
    opacity: 1, brightness: 0, contrast: 1, saturation: 1, volume: 1, playback: 'Loop', visible: true, muted: false,
    rotation: 0, speed: 1, trimInMs: 0, trimOutMs: null, crop: { left: 0, top: 0, right: 0, bottom: 0 },
  };
}

/** `scene` with `layers` on screen `screenId` (`null` drops the screen: the scene no longer touches it). */
export function withScreen(scene: Scene, screenId: string, layers: Layer[] | null): Scene {
  const { [screenId]: _, ...rest } = scene;
  return layers ? { ...rest, [screenId]: layers } : rest;
}

export const findLayer = (scene: Scene, screenId: string, layerId: string) => scene[screenId]?.find((l) => l.id === layerId);

/** `scene` with one layer replaced by `f(layer)`, or removed when `f` returns null. The same
 *  `scene` when it has no such layer: an edit aimed elsewhere must never black out a screen. */
export function withLayer(scene: Scene, screenId: string, layerId: string, f: (l: Layer) => Layer | null): Scene {
  const layers = scene[screenId];
  if (!layers?.some((l) => l.id === layerId)) return scene;
  return withScreen(scene, screenId, layers.flatMap((l) => (l.id === layerId ? (f(l) ?? []) : [l])));
}

/** `scene` with a layer moved `by` places in its screen's stack (positive: up, towards the top;
 *  ±Infinity: to the top or bottom); unchanged when it can't move. */
export function shiftLayer(scene: Scene, screenId: string, layerId: string, by: number): Scene {
  const layers = scene[screenId] ?? [];
  const i = layers.findIndex((l) => l.id === layerId);
  const j = Math.max(0, Math.min(layers.length - 1, i + by));
  if (i < 0 || j === i) return scene;
  const next = layers.filter((_, k) => k !== i);
  next.splice(j, 0, layers[i]);
  return withScreen(scene, screenId, next);
}

/** A cue shortcut from a key press (`A`–`Z`, `0`–`9`, `F1`–`F12`, as the show's `valid_hotkey`
 *  takes them), else null. */
export function cueKey(e: Pick<KeyboardEvent, 'key'>): string | null {
  const k = e.key.length === 1 ? e.key.toUpperCase() : e.key;
  return /^([A-Z0-9]|F([1-9]|1[0-2]))$/.test(k) ? k : null;
}

/** A clip's progress for its canvas box, from the engine's reported position (ms into the file):
 *  elapsed and remaining in real time (the trimmed part, at the layer's speed) and the fraction
 *  played. Null for stills and unprobed media. */
export function clipTime(
  media: MediaItem | undefined,
  layer: Pick<Layer, 'trimInMs' | 'trimOutMs' | 'speed'>,
  pos: number | undefined,
): { elapsed: number; remaining: number; fraction: number } | null {
  const total = media?.durationMs;
  if (!total) return null;
  const start = Math.min(layer.trimInMs, total);
  const length = Math.max(1, Math.min(layer.trimOutMs ?? total, total) - start);
  const played = Math.min(Math.max(0, (pos ?? start) - start), length);
  const speed = layer.speed > 0 ? layer.speed : 1;
  return { elapsed: played / speed, remaining: (length - played) / speed, fraction: played / length };
}

/** The part of a layer's source drawn, as a unit-square box (x, y, w, h), as the engine reads the
 *  crop: one that leaves (almost) nothing is ignored. */
export function cropBox(c: Layer['crop']): [number, number, number, number] {
  const edge = (v: number) => (Number.isFinite(v) ? Math.min(1, Math.max(0, v)) : 0);
  const [x0, y0, x1, y1] = [edge(c.left), edge(c.top), 1 - edge(c.right), 1 - edge(c.bottom)];
  return x1 - x0 < 0.001 || y1 - y0 < 0.001 ? [0, 0, 1, 1] : [x0, y0, x1 - x0, y1 - y0];
}

export type CropSide = 'left' | 'top' | 'right' | 'bottom';

/** A crop handle dragged by `d` rect units (positive: right/down): that side's crop and the
 *  matching edge of `rect` move together, so the picture keeps its place and scale and only what
 *  is cut changes. Stops at no crop on that side, and at 1 unit / 1 % of the source left. */
export function cropDrag(rect: Rect, crop: Layer['crop'], side: CropSide, d: number): { rect: Rect; crop: Layer['crop'] } {
  const across = side === 'left' || side === 'right';
  const size = across ? rect.w : rect.h;
  const kept = across ? 1 - crop.left - crop.right : 1 - crop.top - crop.bottom;
  if (!(size > 0) || !(kept > 0)) return { rect, crop };
  const perUnit = kept / size; // source fraction per rect unit
  const min = -crop[side] / perUnit; // back out to no crop on this side
  const max = size - Math.max(1, 0.01 / perUnit);
  const inward = side === 'left' || side === 'top' ? d : -d;
  const r = Math.round(Math.min(max, Math.max(min, inward)));
  const step = r < min ? min : r > max ? max : r;
  const next = { ...crop, [side]: Math.max(0, crop[side] + step * perUnit) };
  const moved = { ...rect };
  if (across) {
    moved.w = size - step;
    if (side === 'left') moved.x = rect.x + step;
  } else {
    moved.h = size - step;
    if (side === 'top') moved.y = rect.y + step;
  }
  return { rect: moved, crop: next };
}

export type MediaUse = { cues: number; pvw: boolean; pgm: boolean };

/** Where each media item is used: in how many cues, and whether Preview or Program shows it. An
 *  item missing from the map is unused. */
export function mediaUses(show: Show | null, live: Live): Map<string, MediaUse> {
  const uses = new Map<string, MediaUse>();
  const use = (id: string) => uses.get(id) ?? uses.set(id, { cues: 0, pvw: false, pgm: false }).get(id)!;
  const ids = (sc: Scene) => new Set(Object.values(sc).flatMap((ls) => ls.flatMap((l) => (l.source.type === 'Media' ? [l.source.mediaId] : []))));
  for (const c of show?.groups.flatMap((g) => g.cues) ?? []) for (const id of ids(c.layersByScreen)) use(id).cues++;
  for (const id of ids(live.preview)) use(id).pvw = true;
  for (const id of ids(live.program)) use(id).pgm = true;
  return uses;
}

/** The media item a layer plays, if it plays one. */
export function mediaOf(show: Show | null, layer: Layer | undefined): MediaItem | undefined {
  const src = layer?.source;
  return src?.type === 'Media' ? show?.media.find((m) => m.id === src.mediaId) : undefined;
}

/** Whether `after` shows less than `before`: a layer that was showing is removed, hidden or at
 *  opacity 0, or gets another source or playback mode where a clip is involved (the engine opens a
 *  new player, which blanks until its first frame and starts from the top: M6), or a screen the
 *  scene didn't define becomes explicit black. An edit of Program goes to air at once, so such an
 *  edit asks first. */
export function losesPicture(before: Scene, after: Scene): boolean {
  const shown = (sc: Scene, s: string) => (sc[s] ?? []).filter((l) => l.visible && l.opacity > 0);
  const media = (l: Layer) => l.source.type === 'Media';
  const newPlayer = (a: Layer, b: Layer) =>
    (media(a) || media(b)) && (!same(a.source, b.source) || a.playback !== b.playback || a.trimInMs !== b.trimInMs || a.trimOutMs !== b.trimOutMs);
  return Object.entries(after).some(([s, layers]) => {
    const now = new Map(shown(after, s).map((l) => [l.id, l]));
    const lost = (l: Layer) => {
      const n = now.get(l.id);
      return !n || newPlayer(l, n);
    };
    return shown(before, s).some(lost) || (!layers.length && !(s in before));
  });
}

/** An autosave, or one kept from an earlier session (`….autosave.prev`, `….autosave.<ms>.prev`):
 *  it opens as untitled work. */
export const isAutosave = (p: string) => /\.autosave(\.\d+(-\d+)?)?\.prev$|\.autosave$/.test(p);

/** SVG fill for a test pattern in a cue thumbnail: Solid as its colour, the others from the
 *  `cue-…` paint servers CueGrid defines (approximating the engine's layer.wgsl). */
export const patternFill = (p: Pattern) =>
  p.type === 'Solid' ? `rgba(${p.rgba.slice(0, 3).map((c) => Math.round(c * 255)).join(',')},${p.rgba[3]})` : `url(#cue-${p.type.toLowerCase()})`;

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
  if (k === 'enter' && !e.shiftKey) return 'take';
  return null;
}

/** Keys that work on the selected Preview layer: arrows nudge it (1 px, 10 with Shift); `[` / `]`
 *  move it down / up its screen's stack (with ⌘/Ctrl: to the bottom / top). By physical key, so
 *  the brackets work on any keyboard layout. */
export function layerKey(e: KeyLike): { nudge: [number, number] } | { order: number } | null {
  if (e.altKey) return null;
  const mod = e.metaKey || e.ctrlKey;
  const step = e.shiftKey ? 10 : 1;
  const nudge: Record<string, [number, number]> = { ArrowLeft: [-step, 0], ArrowRight: [step, 0], ArrowUp: [0, -step], ArrowDown: [0, step] };
  if (!mod && e.key in nudge) return { nudge: nudge[e.key] };
  if (e.shiftKey) return null;
  if (e.code === 'BracketLeft' || e.key === '[') return { order: mod ? -Infinity : -1 };
  if (e.code === 'BracketRight' || e.key === ']') return { order: mod ? Infinity : 1 };
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
