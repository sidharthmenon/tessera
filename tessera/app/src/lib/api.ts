// Typed wrappers for the controller's Tauri commands and events (app/src-tauri/src/lib.rs).
// Commands return `Result<_, String>`: an `Err` rejects the promise with that string.
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type { Bus } from '$lib/bindings/Bus';
import type { Layer } from '$lib/bindings/Layer';
import type { FromEngine } from '$lib/bindings/FromEngine';
import type { Show } from '$lib/bindings/Show';
import type { ShowOp } from '$lib/bindings/ShowOp';
import type { Transition } from '$lib/bindings/Transition';
import type { TransportAction } from '$lib/bindings/TransportAction';

// App-crate types, not in the generated bindings: controller.rs `CueRef`/`Live`, engine_client.rs `EngineInfo`.
export type CueRef = { groupId: string; slot: number };
/** `program`: what PGM shows, by screen id, with the engine's layer ids (transport targets these). */
/** `pvw`: the cue Preview was last loaded from or stored into. `preview`/`transition`: what PVW
 *  holds, edited freely (no cue changes); `program`: what PGM shows. Scenes by screen id. */
export type Live = { pvw: CueRef | null; lastTaken: CueRef | null; program: Scene; preview: Scene; transition: Transition };
type Scene = { [screenId: string]: Layer[] };
export type EngineInfo = { port: number; token: string; adopted: boolean; held: boolean };

export const getShow = () => invoke<Show>('get_show');
export const getLive = () => invoke<Live>('get_live');
export const applyOp = (op: ShowOp) => invoke<null>('apply_op', { op });
export const undo = () => invoke<null>('undo');
export const redo = () => invoke<null>('redo');
/** Rejects when the slot is empty. */
export const loadCue = (groupId: string, slot: number) => invoke<null>('load_cue', { groupId, slot });
/** An edit of Preview: no cue changes. */
export const setPreview = (scene: Scene, transition: Transition) => invoke<null>('set_preview', { scene, transition });
/** Preview into the cue at the slot (made if the slot is empty). */
export const storePreview = (groupId: string, slot: number) => invoke<null>('store_preview', { groupId, slot });
/** A live edit of Program. */
export const setProgram = (scene: Scene) => invoke<null>('set_program', { scene });
export const take = () => invoke<null>('take');
export const loadNext = () => invoke<null>('load_next');
/** `layer: null` = every player on the bus. */
export const transport = (bus: Bus, layer: string | null, action: TransportAction) =>
  invoke<null>('transport', { bus, layer, action });
/** The reply comes as an `engine-event` `Monitors`. */
export const listMonitors = () => invoke<null>('list_monitors');
/** The reply comes as an `engine-event` `Inputs`. */
export const listInputs = () => invoke<null>('list_inputs');
export const identify = (outputId: string) => invoke<null>('identify', { outputId });
export const importMedia = (paths: string[]) => invoke<null>('import_media', { paths });
export const newShow = () => invoke<null>('new_show');
/** An `.autosave`/`.autosave.prev` opens as an untitled show. */
export const openShow = (path: string) => invoke<null>('open_show', { path });
/** `path: null` saves to the current file (rejects for an untitled show). */
export const saveShow = (path: string | null) => invoke<null>('save_show', { path });
export const engineInfo = () => invoke<EngineInfo | null>('engine_info');
/** Stop the engine (every output goes black) until `engineStart`. */
export const engineStop = () => invoke<null>('engine_stop');
export const engineStart = () => invoke<null>('engine_start');
/** Replace the engine; the new one gets the show, Preview and Program back on connect. */
export const engineRestart = () => invoke<null>('engine_restart');
export const engineStopped = () => invoke<boolean>('engine_stopped');

/** Remote control (Companion, scripts): see docs/external-control.md. */
export type RemoteStatus = { enabled: boolean; running: boolean; port: number; key: string; address: string | null; clients: number; error: string | null };
export type ProcessUsage = { cpu: number; ram: number };
export type Usage = { engine: ProcessUsage | null; app: ProcessUsage; machineCpu: number; ramUsed: number; ramTotal: number };
export const remoteStatus = () => invoke<RemoteStatus>('remote_status');
export const remoteSet = (enabled: boolean, port: number) => invoke<RemoteStatus>('remote_set', { enabled, port });
export const remoteNewKey = () => invoke<RemoteStatus>('remote_new_key');
/** Unsaved work from an earlier session. Read it before any `openShow`, which resets it. */
export const getRecovery = () => invoke<string | null>('get_recovery');

type Events = {
  'show-changed': Show;
  'live-changed': Live;
  'engine-event': FromEngine;
  /** Port and token change on every engine respawn: rebuild preview/thumbnail URLs. */
  'engine-info': EngineInfo | null;
  'recovery-available': string;
  /** The operator stopped (true) or started (false) the engine. */
  'engine-stopped': boolean;
  'remote-changed': RemoteStatus;
  usage: Usage;
};

export const on = <K extends keyof Events>(event: K, f: (payload: Events[K]) => void): Promise<UnlistenFn> =>
  listen<Events[K]>(event, (e) => f(e.payload));
