// Typed wrappers for the controller's Tauri commands and events (app/src-tauri/src/lib.rs).
// Commands return `Result<_, String>`: an `Err` rejects the promise with that string.
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type { Bus } from '$lib/bindings/Bus';
import type { Layer } from '$lib/bindings/Layer';
import type { FromEngine } from '$lib/bindings/FromEngine';
import type { Show } from '$lib/bindings/Show';
import type { ShowOp } from '$lib/bindings/ShowOp';
import type { TransportAction } from '$lib/bindings/TransportAction';

// App-crate types, not in the generated bindings: controller.rs `CueRef`/`Live`, engine_client.rs `EngineInfo`.
export type CueRef = { groupId: string; slot: number };
/** `program`: what PGM shows, by screen id, with the engine's layer ids (transport targets these). */
export type Live = { pvw: CueRef | null; lastTaken: CueRef | null; program: { [screenId: string]: Layer[] } };
export type EngineInfo = { port: number; token: string; adopted: boolean; held: boolean };

export const getShow = () => invoke<Show>('get_show');
export const getLive = () => invoke<Live>('get_live');
export const applyOp = (op: ShowOp) => invoke<null>('apply_op', { op });
export const undo = () => invoke<null>('undo');
export const redo = () => invoke<null>('redo');
/** Rejects when the slot is empty. */
export const loadCue = (groupId: string, slot: number) => invoke<null>('load_cue', { groupId, slot });
export const take = () => invoke<null>('take');
export const loadNext = () => invoke<null>('load_next');
/** `layer: null` = every player on the bus. */
export const transport = (bus: Bus, layer: string | null, action: TransportAction) =>
  invoke<null>('transport', { bus, layer, action });
/** The reply comes as an `engine-event` `Monitors`. */
export const listMonitors = () => invoke<null>('list_monitors');
export const identify = (outputId: string) => invoke<null>('identify', { outputId });
export const importMedia = (paths: string[]) => invoke<null>('import_media', { paths });
export const newShow = () => invoke<null>('new_show');
/** An `.autosave`/`.autosave.prev` opens as an untitled show. */
export const openShow = (path: string) => invoke<null>('open_show', { path });
/** `path: null` saves to the current file (rejects for an untitled show). */
export const saveShow = (path: string | null) => invoke<null>('save_show', { path });
export const engineInfo = () => invoke<EngineInfo | null>('engine_info');
/** Unsaved work from an earlier session. Read it before any `openShow`, which resets it. */
export const getRecovery = () => invoke<string | null>('get_recovery');

type Events = {
  'show-changed': Show;
  'live-changed': Live;
  'engine-event': FromEngine;
  /** Port and token change on every engine respawn: rebuild preview/thumbnail URLs. */
  'engine-info': EngineInfo | null;
  'recovery-available': string;
};

export const on = <K extends keyof Events>(event: K, f: (payload: Events[K]) => void): Promise<UnlistenFn> =>
  listen<Events[K]>(event, (e) => f(e.payload));
