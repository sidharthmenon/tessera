// Pure helpers for the main window: no runes, no Tauri, so `npm test` runs them in plain Node.
import type { Cue } from './bindings/Cue';
import type { Group } from './bindings/Group';
import type { Layer } from './bindings/Layer';
import type { MediaItem } from './bindings/MediaItem';
import type { Rect } from './bindings/Rect';
import type { Screen } from './bindings/Screen';
import type { Show } from './bindings/Show';
import type { Source } from './bindings/Source';
import type { Transition } from './bindings/Transition';
import type { CueRef, EngineInfo } from './api';

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

/** `cue` with one layer replaced by `f(layer)`, or removed when `f` returns null. */
export function withLayer(cue: Cue, screenId: string, layerId: string, f: (l: Layer) => Layer | null): Cue {
  const layers = cue.layersByScreen[screenId] ?? [];
  return withScreen(cue, screenId, layers.flatMap((l) => (l.id === layerId ? (f(l) ?? []) : [l])));
}

/** The media item a layer plays, if it plays one. */
export function mediaOf(show: Show | null, layer: Layer | undefined): MediaItem | undefined {
  const src = layer?.source;
  return src?.type === 'Media' ? show?.media.find((m) => m.id === src.mediaId) : undefined;
}

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

/** Focus where keys belong to the element (text entry, a select), not to the show hotkeys.
 *  Checkboxes, sliders and buttons don't count: Space there must still be GO. */
export function typingIn(el: { tagName?: string; type?: string; isContentEditable?: boolean } | null): boolean {
  if (!el) return false;
  if (el.tagName === 'TEXTAREA' || el.tagName === 'SELECT' || el.isContentEditable) return true;
  return el.tagName === 'INPUT' && !['checkbox', 'radio', 'range', 'button', 'color', 'submit', 'reset'].includes(el.type ?? 'text');
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
