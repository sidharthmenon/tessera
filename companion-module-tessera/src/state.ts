// Pure functions from Tessera's state to Companion's variables, feedback checks and the per-cue
// presets: no Companion API here, so they are unit-tested directly.

import type { Snapshot } from './types.js'

/** `m:ss` (or `h:mm:ss`) for a duration in ms; "" for none. */
export function clock(ms: number | null | undefined): string {
	if (ms == null || !Number.isFinite(ms)) return ''
	const total = Math.max(0, Math.round(ms / 1000))
	const [h, m, s] = [Math.floor(total / 3600), Math.floor((total % 3600) / 60), total % 60]
	const pad = (n: number) => String(n).padStart(2, '0')
	return h ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`
}

/** The variable holding cue `cue` of group `group`'s name. */
export const cueVariable = (group: number, cue: number): string => `cue_${group}_${cue}`

export interface VariableDefinition {
	variableId: string
	name: string
}

const FIXED: VariableDefinition[] = [
	{ variableId: 'show', name: 'Show name' },
	{ variableId: 'engine', name: 'Engine status' },
	{ variableId: 'volume', name: 'Master volume (0–100)' },
	{ variableId: 'pvw_group', name: 'Preview: group number' },
	{ variableId: 'pvw_cue', name: 'Preview: cue number' },
	{ variableId: 'pvw_name', name: 'Preview: cue name' },
	{ variableId: 'pgm_group', name: 'Program: group number' },
	{ variableId: 'pgm_cue', name: 'Program: cue number' },
	{ variableId: 'pgm_name', name: 'Program: cue name' },
	{ variableId: 'pgm_clip', name: 'Program: clip that ends last' },
	{ variableId: 'pgm_elapsed', name: 'Program: clip time elapsed (m:ss)' },
	{ variableId: 'pgm_remaining', name: 'Program: clip time remaining (m:ss)' },
]

/** The fixed variables plus one per cue (its name). */
export function variableDefinitions(state: Snapshot | null): VariableDefinition[] {
	const cues = (state?.groups ?? []).flatMap((g) =>
		g.cues.map((c) => ({ variableId: cueVariable(g.number, c.number), name: `Cue ${c.number} of ${g.name}: name` })),
	)
	return [...FIXED, ...cues]
}

export function variableValues(state: Snapshot): Record<string, string | number | undefined> {
	const values: Record<string, string | number | undefined> = {
		show: state.show,
		engine: state.engine,
		volume: state.volume,
		pvw_group: state.preview.group ?? '',
		pvw_cue: state.preview.cue ?? '',
		pvw_name: state.preview.name ?? (state.preview.empty ? '' : 'not stored'),
		pgm_group: state.program.group ?? '',
		pgm_cue: state.program.cue ?? '',
		pgm_name: state.program.name ?? (state.program.empty ? '' : 'live'),
		pgm_clip: state.time?.clip ?? '',
		pgm_elapsed: clock(state.time?.elapsedMs),
		pgm_remaining: clock(state.time?.remainingMs),
	}
	for (const g of state.groups) for (const c of g.cues) values[cueVariable(g.number, c.number)] = c.name
	return values
}

/** A change in the groups/cues (not just which is live) needs new variable and preset definitions. */
export function cueShape(state: Snapshot): string {
	return JSON.stringify(state.groups.map((g) => [g.name, g.cues.map((c) => c.number)]))
}

export const inPreview = (state: Snapshot | null, group: number, cue: number): boolean =>
	!!state && state.preview.group === group && state.preview.cue === cue

export const onProgram = (state: Snapshot | null, group: number, cue: number): boolean =>
	!!state && state.program.group === group && state.program.cue === cue

export const previewEdited = (state: Snapshot | null): boolean => !!state?.preview.edited

export const engineConnected = (state: Snapshot | null): boolean => state?.engine === 'connected'
