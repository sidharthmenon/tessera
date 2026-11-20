// `npm test`: plain Node (type stripping), no test framework. Throws on the first failure.
import type { Cue } from './bindings/Cue';
import type { Group } from './bindings/Group';
import type { Screen } from './bindings/Screen';
import * as L from './logic.ts';

function eq(got: unknown, want: unknown, what: string) {
  const [g, w] = [JSON.stringify(got), JSON.stringify(want)];
  if (g !== w) throw new Error(`${what}: got ${g}, want ${w}`);
}

const scr = (id: string, layoutX: number, layoutY: number, width = 100, height = 50): Screen => ({ id, name: id, width, height, layoutX, layoutY });
const a = scr('a', 0, 0);
const b = scr('b', 100, 0, 100, 100);

// Preview extent matches the engine's render_layout, including negative layout positions.
eq(L.layoutBounds([]), { x: 0, y: 0, w: 1, h: 1 }, 'empty bounds');
eq(L.layoutBounds([a, b]), { x: 0, y: 0, w: 200, h: 100 }, 'bounds');
eq(L.layoutBounds([scr('n', -50, -10)]), { x: -50, y: -10, w: 100, h: 50 }, 'negative bounds');
eq(L.fitScale({ x: 0, y: 0, w: 200, h: 100 }, 400, 400), 2, 'fit by width');
eq(L.fitScale({ x: 0, y: 0, w: 200, h: 100 }, 1000, 50), 0.5, 'fit by height');

// Drop point → screen; boxes in display pixels; the topmost screen wins where they overlap.
eq(L.screenAt([a, b], 150, 80)?.id, 'b', 'screen under point');
eq(L.screenAt([a, b], 50, 80), undefined, 'gap below a');
eq(L.screenAt([a, scr('over', 50, 0)], 60, 10)?.id, 'over', 'topmost wins');
eq(L.boxOf({ x: 10, y: 5, w: 20, h: 10 }, b, L.layoutBounds([a, b]), 2), { x: 220, y: 10, w: 40, h: 20 }, 'box on screen b');
eq(L.boxOf({ x: 0, y: 0, w: 1, h: 1 }, scr('n', -50, -10), L.layoutBounds([scr('n', -50, -10)]), 1), { x: 0, y: 0, w: 1, h: 1 }, 'box at origin');

eq(L.dragRect({ x: 10, y: 10, w: 50, h: 50 }, 5.4, -3.6, 'move'), { x: 15, y: 6, w: 50, h: 50 }, 'move rounds');
eq(L.dragRect({ x: 10, y: 10, w: 50, h: 50 }, -80, 10, 'resize'), { x: 10, y: 10, w: 1, h: 60 }, 'resize keeps >= 1');

// Slots.
const cue = (slot: number, layersByScreen: Cue['layersByScreen'] = {}): Cue => L.newCue(slot, layersByScreen);
const g: Group = { id: 'g', name: 'G', cues: [cue(0), cue(1), cue(3)] };
eq(L.firstEmptySlot(g), 2, 'first gap');
eq(L.firstEmptySlot(g, 1), 2, 'after 1');
eq(L.firstEmptySlot(g, 3), 4, 'after last');
eq(L.slotCount({ ...g, cues: [] }), 27, 'min 27');
eq(L.slotCount({ ...g, cues: [cue(26)] }), 36, 'a free row after slot 27');
eq(L.cueAt({ version: 1, outputs: [], screens: [], mappings: [], media: [], groups: [g], masterVolume: 1 }, { groupId: 'g', slot: 3 })?.slot, 3, 'cueAt');
eq(L.sameRef({ groupId: 'g', slot: 1 }, { groupId: 'g', slot: 1 }), true, 'sameRef');
eq(L.sameRef(null, null), false, 'null refs differ (empty PVW is not live)');

// Cue edits are immutable; copies get fresh layer ids; a dropped screen key means "untouched".
const layer = L.newLayer({ type: 'Pattern', pattern: { type: 'Bars' } }, b, 'Bars');
eq(layer.rect, { x: 0, y: 0, w: 100, h: 100 }, 'full-screen layer');
const c = cue(5, { b: [layer] });
const copy = L.copyScene(c.layersByScreen);
eq(copy.b.length === 1 && copy.b[0].id !== layer.id && copy.b[0].name === 'Bars', true, 'copy has fresh ids');
const hidden = L.withLayer(c, 'b', layer.id, (l) => ({ ...l, visible: false }));
eq([hidden.layersByScreen.b[0].visible, c.layersByScreen.b[0].visible], [false, true], 'withLayer is immutable');
eq(L.withLayer(c, 'b', layer.id, () => null).layersByScreen, { b: [] }, 'removing the last layer leaves explicit black');
eq(L.withScreen(c, 'b', null).layersByScreen, {}, 'released screen');
// R41: an edit aimed at a layer the cue doesn't hold (e.g. one an earlier cue left on PGM) changes nothing.
eq(L.withLayer(c, 'a', layer.id, () => null) === c && L.withLayer(c, 'b', 'other', () => null) === c, true, 'foreign layer: cue unchanged');
eq(L.findLayer(c.layersByScreen, 'b', layer.id)?.id === layer.id && !L.findLayer(c.layersByScreen, 'a', layer.id), true, 'findLayer');
eq(Object.keys(L.withScreen(c, 'a', []).layersByScreen).sort(), ['a', 'b'], 'cleared screen is defined');
// Review fix: which edits take picture off the on-air cue (and so ask first).
{
  const hit = (f: (c: Cue) => Cue) => L.losesPicture(c, f(c));
  const lay = (patch: object) => (x: Cue) => L.withLayer(x, 'b', layer.id, (l) => ({ ...l, ...patch }));
  eq(
    [hit((x) => L.withLayer(x, 'b', layer.id, () => null)), hit(lay({ visible: false })), hit(lay({ opacity: 0 })), hit((x) => L.withScreen(x, 'b', [])), hit((x) => L.withScreen(x, 'a', []))],
    [true, true, true, true, true], 'remove, hide, opacity 0, clear, black a new screen',
  );
  const hidden = lay({ visible: false })(c);
  eq(
    [hit((x) => L.withScreen(x, 'b', null)), hit(lay({ rect: { x: 5, y: 5, w: 10, h: 10 } })), hit(lay({ opacity: 0.5, name: 'x' })), hit((x) => ({ ...x, name: 'renamed' })),
      hit((x) => L.withScreen(x, 'b', [...x.layersByScreen.b, L.newLayer({ type: 'Pattern', pattern: { type: 'Grid' } }, b, 'Grid')])), L.losesPicture(hidden, c), L.losesPicture(hidden, L.withLayer(hidden, 'b', layer.id, () => null))],
    [false, false, false, false, false, false, false], 'leave screen, move, dim, rename, add, unhide, remove an already hidden layer',
  );
}
eq(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(L.uuid()), true, 'uuid v4');

// Hotkeys, and when they're off.
const k = (key: string, m: Partial<{ metaKey: boolean; ctrlKey: boolean; shiftKey: boolean; altKey: boolean; code: string }> = {}) =>
  L.hotkey({ key, metaKey: false, ctrlKey: false, shiftKey: false, altKey: false, ...m });
eq([k(' ', { code: 'Space' }), k('ArrowRight'), k('z', { metaKey: true }), k('Z', { metaKey: true, shiftKey: true }), k('s', { ctrlKey: true }), k('S', { metaKey: true, shiftKey: true })],
  ['take', 'next', 'undo', 'redo', 'save', 'saveAs'], 'hotkeys');
eq([k('z'), k(' ', { shiftKey: true }), k('ArrowRight', { metaKey: true }), k('z', { metaKey: true, altKey: true })], [null, null, null, null], 'not hotkeys');
// R40: any focused control keeps the keys (Space must never both work it and GO).
const el = (tagName: string, role: string | null = null, isContentEditable = false) => ({ tagName, isContentEditable, getAttribute: (n: string) => (n === 'role' ? role : null) });
eq(
  [el('BUTTON'), el('INPUT'), el('SELECT'), el('TEXTAREA'), el('DIV', null, true), el('DIV', 'slider'), el('SPAN', 'button')].map(L.ownsKeys),
  [true, true, true, true, true, true, true], 'controls own the keys',
);
eq([null, el('BODY'), el('DIV'), el('DIV', 'img'), { tagName: 'DIV' }].map(L.ownsKeys), [false, false, false, false, false], 'nothing focused: show keys');

eq([L.fmtTime(83500), L.fmtTime(59960), L.fmtTime(0)], ['1:23.5', '1:00.0', '0:00.0'], 'fmtTime');
eq(L.hexToRgba('#ff0080').map((v) => +v.toFixed(3)), [1, 0, 0.502, 1], 'hex → rgba');
eq(L.rgbaToHex([1, 0, 0.502, 1]), '#ff0080', 'rgba → hex');
eq(L.engineUrl({ port: 5, token: 'ab', adopted: false, held: false }, '/preview/pvw.mjpg'), 'http://127.0.0.1:5/preview/pvw.mjpg?token=ab', 'engine url');
eq(L.engineUrl(null, '/x'), null, 'no engine, no url');

console.log('logic: all checks passed');
