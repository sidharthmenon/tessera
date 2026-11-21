// The state Tessera's remote-control API sends (`GET /api/v1/state`, and each WebSocket message's
// `state`). Mirrors `Snapshot` in tessera/app/src-tauri/src/remote.rs.

export interface Snapshot {
	/** The show file's name, or "Untitled show". */
	show: string
	/** `connected`, `held`, `connecting` or `stopped`. */
	engine: string
	groups: GroupInfo[]
	preview: BusInfo
	program: BusInfo
	/** Master volume, 0–100. */
	volume: number
	/** The Program clip that ends last, while one plays. */
	time: ClipTime | null
}

export interface GroupInfo {
	/** 1-based, as on screen. */
	number: number
	name: string
	cues: CueInfo[]
}

export interface CueInfo {
	/** 1-based slot number, as on screen. */
	number: number
	name: string
	hotkey: string | null
}

export interface BusInfo {
	group: number | null
	cue: number | null
	name: string | null
	/** Shows nothing. */
	empty: boolean
	/** Preview only: changed since its cue was loaded. */
	edited: boolean
}

export interface ClipTime {
	elapsedMs: number
	remainingMs: number
	clip: string
}

export interface Config {
	host: string
	port: number
	key: string
}

export const DEFAULT_PORT = 7410
