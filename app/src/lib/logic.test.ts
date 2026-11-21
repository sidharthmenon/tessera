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
const cue = (slot: number, layersByScreen: L.Scene = {}): Cue => ({ id: `c${slot}`, name: `Cue ${slot + 1}`, slot, layersByScreen, transition: { kind: 'Fade', durationMs: 500 }, hotkey: null });
const g: Group = { id: 'g', name: 'G', cues: [cue(0), cue(1), cue(3)] };
eq(L.firstEmptySlot(g), 2, 'first gap');
eq(L.firstEmptySlot(g, 1), 2, 'after 1');
eq(L.firstEmptySlot(g, 3), 4, 'after last');
eq(L.slotCount({ ...g, cues: [] }), 27, 'min 27');
eq(L.slotCount({ ...g, cues: [cue(26)] }), 36, 'a free row after slot 27');
eq(L.cueAt({ version: 1, outputs: [], screens: [], mappings: [], media: [], groups: [g], masterVolume: 1 }, { groupId: 'g', slot: 3 })?.slot, 3, 'cueAt');
eq(L.sameRef({ groupId: 'g', slot: 1 }, { groupId: 'g', slot: 1 }), true, 'sameRef');
eq(L.sameRef(null, null), false, 'null refs differ (empty PVW is not live)');

// Scene edits are immutable; a dropped screen key means "untouched".
const layer = L.newLayer({ type: 'Pattern', pattern: { type: 'Bars' } }, b, 'Bars');
eq(layer.rect, { x: 0, y: 0, w: 100, h: 100 }, 'full-screen layer');
const c: L.Scene = { b: [layer] };
const hidden = L.withLayer(c, 'b', layer.id, (l) => ({ ...l, visible: false }));
eq([hidden.b[0].visible, c.b[0].visible], [false, true], 'withLayer is immutable');
eq(L.withLayer(c, 'b', layer.id, () => null), { b: [] }, 'removing the last layer leaves explicit black');
eq(L.withScreen(c, 'b', null), {}, 'released screen');
// An edit aimed at a layer the scene doesn't hold changes nothing.
eq(L.withLayer(c, 'a', layer.id, () => null) === c && L.withLayer(c, 'b', 'other', () => null) === c, true, 'foreign layer: scene unchanged');
eq(L.findLayer(c, 'b', layer.id)?.id === layer.id && !L.findLayer(c, 'a', layer.id), true, 'findLayer');
eq(Object.keys(L.withScreen(c, 'a', [])).sort(), ['a', 'b'], 'cleared screen is defined');
// Which Program edits take picture off air (and so ask first).
{
  const hit = (f: (sc: L.Scene) => L.Scene) => L.losesPicture(c, f(c));
  const lay = (patch: object) => (x: L.Scene) => L.withLayer(x, 'b', layer.id, (l) => ({ ...l, ...patch }));
  eq(
    [hit((x) => L.withLayer(x, 'b', layer.id, () => null)), hit(lay({ visible: false })), hit(lay({ opacity: 0 })), hit((x) => L.withScreen(x, 'b', [])), hit((x) => L.withScreen(x, 'a', []))],
    [true, true, true, true, true], 'remove, hide, opacity 0, clear, black a new screen',
  );
  const hidden = lay({ visible: false })(c);
  eq(
    [hit((x) => L.withScreen(x, 'b', null)), hit(lay({ rect: { x: 5, y: 5, w: 10, h: 10 } })), hit(lay({ opacity: 0.5, name: 'x' })),
      hit((x) => L.withScreen(x, 'b', [...x.b, L.newLayer({ type: 'Pattern', pattern: { type: 'Grid' } }, b, 'Grid')])), L.losesPicture(hidden, c), L.losesPicture(hidden, L.withLayer(hidden, 'b', layer.id, () => null))],
    [false, false, false, false, false, false], 'leave screen, move, dim and rename, add, unhide, remove an already hidden layer',
  );
}
// M6: on Program, a source or playback change of a showing media layer recreates its player.
{
  const vid = L.newLayer({ type: 'Media', mediaId: 'm1' }, b, 'Clip');
  const cv: L.Scene = { b: [vid, layer] };
  const set = (id: string, patch: object) => (x: L.Scene) => L.withLayer(x, 'b', id, (l) => ({ ...l, ...patch }));
  const hit = (f: (sc: L.Scene) => L.Scene) => L.losesPicture(cv, f(cv));
  eq(
    [hit(set(vid.id, { source: { type: 'Media', mediaId: 'm2' } })), hit(set(vid.id, { playback: 'Once' })), hit(set(layer.id, { source: { type: 'Media', mediaId: 'm1' } })),
      hit(set(vid.id, { source: { type: 'Pattern', pattern: { type: 'Grid' } } }))],
    [true, true, true, true], 'other clip, playback mode, pattern ↔ media',
  );
  const hiddenVid = set(vid.id, { visible: false })(cv);
  eq(
    [hit(set(layer.id, { source: { type: 'Pattern', pattern: { type: 'Grid' } } })), hit(set(vid.id, { volume: 0.5, rect: { x: 1, y: 1, w: 5, h: 5 } })),
      L.losesPicture(hiddenVid, set(vid.id, { source: { type: 'Media', mediaId: 'm2' } })(hiddenVid))],
    [false, false, false], 'pattern → pattern, volume/move, a hidden clip',
  );
}


// Autosave files open as untitled work (I8 adds `.autosave.<ms>.prev`).
eq(['/s/a.tshow.autosave', '/s/a.tshow.autosave.prev', '/s/a.tshow.autosave.1763680000000.prev', '/s/a.tshow.autosave.1763680000000-2.prev'].map(L.isAutosave), [true, true, true, true], 'autosaves');
eq(['/s/a.tshow', '/s/x.prev', '/s/autosave.tshow'].map(L.isAutosave), [false, false, false], 'not autosaves');

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

// Screen Management (T13): numeric fields, defaults, and the ops Apply sends.
eq([L.parseNum('12.5'), L.parseNum(' -3 '), L.parseNum('1920', { int: true, min: 1 })], [12.5, -3, 1920], 'valid numbers');
eq(
  ['', 'abc', 'NaN', 'Infinity', '1e7', '0', '1.5'].map((t, i) => typeof L.parseNum(t, i >= 5 ? { int: true, min: 1 } : {})),
  ['string', 'string', 'string', 'string', 'string', 'string', 'string'], 'invalid numbers give a message',
);
eq([L.parseNum('0.5', { min: 1 }), L.parseNum('2e6', { int: true })], ['Enter a number from 1 to 1000000', 'Enter a whole number from -1000000 to 1000000'], 'message names the rule');
eq(L.same({ a: 1, b: { c: [1, 2] } }, { b: { c: [1, 2] }, a: 1 }) && !L.same({ a: 1 }, { a: 1, b: 2 }) && !L.same([1, 2], [2, 1]), true, 'deep equal, key order ignored');
{
  const live = { pvw: null, lastTaken: null, program: {}, preview: {}, transition: { kind: 'Cut' as const, durationMs: 0 } };
  eq(L.onAir(live), false, 'nothing on air');
  eq([L.onAir({ ...live, lastTaken: { groupId: 'g', slot: 0 } }), L.onAir({ ...live, program: { s: [] } })], [true, true], 'taken cue or PGM content is on air');
}
{
  const mon = { name: 'LED', x: 1920, y: 0, width: 3840, height: 1080, scale: 1, refreshMhz: 60000 };
  const d = L.displayOutput(mon);
  eq([d.node, d.name, d.target, d.width, d.height, d.brightness], ['local', 'LED', { type: 'Display', monitorName: 'LED', monitorX: 1920, monitorY: 0 }, 3840, 1080, 1], 'display output binds by name + position at the monitor size');
  const w = L.windowOutput('Win', 10, 20, 960, 540);
  eq([w.node, w.target, w.width, w.height], ['local', { type: 'Window', x: 10, y: 20 }, 960, 540], 'window output');
  const s1 = L.newScreen([]);
  eq([s1.name, s1.width, s1.height, s1.layoutX, s1.layoutY], ['Screen 1', 1920, 1080, 0, 0], 'first screen');
  const s2 = L.newScreen([s1, { ...s1, id: 'x', name: 'Screen 2', layoutX: -500, layoutY: 40 }]);
  eq([s2.name, s2.layoutX, s2.layoutY], ['Screen 3', 1920, 0], 'next screen right of the layout');
  const m = L.newMapping(s1, w);
  eq([m.screenId, m.outputId, m.src, m.dst], [s1.id, w.id, { x: 0, y: 0, w: 1920, h: 1080 }, { x: 0, y: 0, w: 960, h: 540 }], 'mapping: full screen → full output');

  const before: L.Setup = { outputs: [d], screens: [s1], mappings: [L.newMapping(s1, d)] };
  const copy = structuredClone(before);
  eq(L.setupOps(before, copy), [], 'unchanged: no ops');
  // Same values, other key order (a rebuilt target): still unchanged.
  copy.outputs[0] = { ...copy.outputs[0], target: { monitorY: 0, monitorX: 1920, monitorName: 'LED', type: 'Display' } };
  eq(L.setupOps(before, copy), [], 'key order is not a change');

  // Everything new: outputs and screens exist before the mappings that use them.
  const empty: L.Setup = { outputs: [], screens: [], mappings: [] };
  eq(L.setupOps(empty, before).map((o) => o.op), ['AddOutput', 'AddScreen', 'SetMapping'], 'adds in dependency order');

  // Remove the screen (the buffer drops its mappings too), add a window output and a new screen
  // mapped onto it, rename the display, move a mapping: mapping removes come first, then the
  // output/screen removes, then adds and updates, mappings last.
  const after: L.Setup = structuredClone(before);
  after.screens = [s2];
  after.mappings = [L.newMapping(s2, w)];
  after.outputs = [{ ...d, name: 'Wall' }, w];
  const ops = L.setupOps(before, after);
  eq(ops.map((o) => o.op), ['RemoveMapping', 'RemoveScreen', 'AddOutput', 'UpdateOutput', 'AddScreen', 'SetMapping'], 'mixed edit order');
  eq(ops[0].op === 'RemoveMapping' && ops[0].id === before.mappings[0].id && ops[1].op === 'RemoveScreen' && ops[1].id === s1.id, true, 'removes name the right ids');
  const moved = structuredClone(before);
  moved.mappings[0].dst.x = 100;
  eq(L.setupOps(before, moved), [{ op: 'SetMapping', mapping: moved.mappings[0] }], 'changed mapping: one SetMapping');
  const resized = structuredClone(before);
  resized.screens[0].width = 3840;
  eq(L.setupOps(before, resized).map((o) => o.op), ['UpdateScreen'], 'screen resize: one UpdateScreen');
  const gone = { ...before, outputs: [], mappings: [] };
  eq(L.setupOps(before, gone).map((o) => o.op), ['RemoveMapping', 'RemoveOutput'], 'output removed after its mapping');
}

// Cue thumbnails paint patterns like the engine: Solid as its colour, the rest from CueGrid's defs.
eq(L.patternFill({ type: 'Solid', rgba: [1, 0.5, 0, 0.25] }), 'rgba(255,128,0,0.25)', 'solid fill');
{
  const xs = [0, 1920, 960];
  const ys = [0, 1080];
  eq(L.snapRect({ x: 6, y: 500, w: 100, h: 100 }, 'move', xs, ys, 10), { x: 0, y: 500, w: 100, h: 100 }, 'snap left edge to output edge');
  eq(L.snapRect({ x: 1815, y: 975, w: 100, h: 100 }, 'move', xs, ys, 10), { x: 1820, y: 980, w: 100, h: 100 }, 'snap right and bottom edges');
  eq(L.snapRect({ x: 955, y: 0, w: 100, h: 100 }, 'move', xs, ys, 10), { x: 960, y: 0, w: 100, h: 100 }, 'snap to another slice edge');
  eq(L.snapRect({ x: 30, y: 30, w: 100, h: 100 }, 'move', xs, ys, 10), { x: 30, y: 30, w: 100, h: 100 }, 'no snap beyond tolerance');
  eq(L.snapRect({ x: 0, y: 0, w: 1912, h: 1073 }, 'resize', xs, ys, 10), { x: 0, y: 0, w: 1920, h: 1080 }, 'resize snaps right and bottom');
  // At 0.5 display px per unit, 8 display px = 16 units.
  const other = { x: 1000, y: 0, w: 500, h: 500 };
  eq(L.snapToEdges({ x: 14, y: 400, w: 100, h: 100 }, 'move', [other], 1920, 1080, 0.5), { x: 0, y: 400, w: 100, h: 100 }, 'snap to frame edge');
  eq(L.snapToEdges({ x: 1510, y: 600, w: 100, h: 100 }, 'move', [other], 1920, 1080, 0.5), { x: 1500, y: 600, w: 100, h: 100 }, 'snap to another rect');
  eq(L.snapToEdges({ x: 20, y: 400, w: 100, h: 100 }, 'move', [other], 1920, 1080, 0.5), { x: 20, y: 400, w: 100, h: 100 }, 'no snap past 8 display px');
}
{
  const lay = (id: string) => ({ ...L.newLayer({ type: 'Pattern', pattern: { type: 'Bars' } }, { id: 's', name: 's', width: 10, height: 10, layoutX: 0, layoutY: 0 }, id), id });
  const c: L.Scene = { s: [lay('a'), lay('b'), lay('c')] };
  const order = (x: L.Scene) => x.s.map((l) => l.id).join('');
  eq(order(L.shiftLayer(c, 's', 'a', 1)), 'bac', 'layer up');
  eq(order(L.shiftLayer(c, 's', 'c', -1)), 'acb', 'layer down');
  eq(L.shiftLayer(c, 's', 'c', 1), c, 'top layer stays');
  eq(L.shiftLayer(c, 's', 'a', -1), c, 'bottom layer stays');
}
eq(L.lockAspect({ x: 5, y: 5, w: 400, h: 100 }, 16 / 9), { x: 5, y: 5, w: 400, h: 225 }, 'aspect: too wide grows height');
eq(L.lockAspect({ x: 5, y: 5, w: 100, h: 900 }, 16 / 9), { x: 5, y: 5, w: 1600, h: 900 }, 'aspect: too tall grows width');
eq(L.lockAspect({ x: 0, y: 0, w: 400, h: 900 }, 16 / 9, 'w'), { x: 0, y: 0, w: 400, h: 225 }, 'aspect by width');
eq(L.snapDrag({ x: 0, y: 0, w: 1000, h: 200 }, 'resize', [], 1920, 1080, 1, 16 / 9), { x: 0, y: 0, w: 1000, h: 563 }, 'locked resize keeps the shape');
eq(L.snapDrag({ x: 0, y: 0, w: 1914, h: 200 }, 'resize', [], 1920, 1080, 1, 16 / 9), { x: 0, y: 0, w: 1920, h: 1080 }, 'locked resize snaps a side, the other follows');
eq(L.snapDrag({ x: 0, y: 0, w: 1000, h: 200 }, 'resize', [], 1920, 1080, 1), { x: 0, y: 0, w: 1000, h: 200 }, 'free resize');
eq(L.fitInto(16 / 9, 2360, 1640), { x: 0, y: 156, w: 2360, h: 1328 }, 'fit 16:9 into a taller output');
eq(L.fitInto(16 / 9, 3840, 1080), { x: 960, y: 0, w: 1920, h: 1080 }, 'fit 16:9 into a wider output');
{
  const lay = (mediaId: string) => L.newLayer({ type: 'Media', mediaId }, b, mediaId);
  const show = { version: 1, outputs: [], screens: [], mappings: [], media: [], masterVolume: 1,
    groups: [{ id: 'g', name: 'G', cues: [cue(0, { b: [lay('m1'), lay('m2')] }), cue(1, { b: [lay('m1')] })] }] };
  const live = { pvw: null, lastTaken: null, program: { b: [lay('m3')] }, preview: { b: [lay('m1')] }, transition: { kind: 'Cut' as const, durationMs: 0 } };
  const uses = L.mediaUses(show, live);
  eq([uses.get('m1'), uses.get('m2'), uses.get('m3'), uses.has('m4')], [{ cues: 2, pvw: true, pgm: false }, { cues: 1, pvw: false, pgm: false }, { cues: 0, pvw: false, pgm: true }, false], 'media uses: cues, Preview, Program, unused');
}
eq(['h', 'H', '7', 'F1', 'F12'].map((key) => L.cueKey({ key })), ['H', 'H', '7', 'F1', 'F12'], 'cue keys, letters upper-cased');
eq([' ', 'ArrowRight', 'Enter', 'Escape', 'F13', 'é', '/'].map((key) => L.cueKey({ key })), [null, null, null, null, null, null, null], 'keys a cue cannot take');
eq(L.patternFill({ type: 'Bars' }), 'url(#cue-bars)', 'bars fill');

console.log('logic: all checks passed');
