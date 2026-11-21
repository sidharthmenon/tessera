import type { CompanionActionDefinitions, CompanionInputFieldNumber } from '@companion-module/base'
import type { TesseraInstance } from './main.js'

const groupField: CompanionInputFieldNumber = { type: 'number', id: 'group', label: 'Group (1 = first tab)', default: 1, min: 1, max: 99 }
const cueField: CompanionInputFieldNumber = { type: 'number', id: 'cue', label: 'Cue number', default: 1, min: 1, max: 999 }

export function actions(self: TesseraInstance): CompanionActionDefinitions {
	/** Runs a command; a refusal (e.g. "Preview is empty") goes to the connection's log. */
	const post = async (path: string) => {
		try {
			await self.client?.post(path)
		} catch (e) {
			self.log('warn', `${path}: ${(e as Error).message}`)
		}
	}
	const n = (v: unknown) => Math.max(1, Math.round(Number(v) || 1))
	return {
		go: {
			name: 'GO (Preview to Program)',
			options: [],
			callback: () => post('/go'),
		},
		load_cue: {
			name: 'Load cue into Preview',
			options: [groupField, cueField],
			callback: (e) => post(`/cue/${n(e.options.group)}/${n(e.options.cue)}/load`),
		},
		fire_cue: {
			name: 'Fire cue (load + GO)',
			options: [groupField, cueField],
			callback: (e) => post(`/cue/${n(e.options.group)}/${n(e.options.cue)}/fire`),
		},
		next: {
			name: 'Load the next cue',
			options: [],
			callback: () => post('/next'),
		},
		previous: {
			name: 'Load the previous cue',
			options: [],
			callback: () => post('/previous'),
		},
		transport: {
			name: 'Playback (Program or Preview)',
			options: [
				{
					type: 'dropdown',
					id: 'bus',
					label: 'Bus',
					default: 'program',
					choices: [
						{ id: 'program', label: 'Program' },
						{ id: 'preview', label: 'Preview' },
					],
				},
				{
					type: 'dropdown',
					id: 'action',
					label: 'Action',
					default: 'play',
					choices: [
						{ id: 'play', label: 'Play' },
						{ id: 'pause', label: 'Pause' },
						{ id: 'stop', label: 'Stop (back to the first frame)' },
						{ id: 'restart', label: 'Restart' },
					],
				},
			],
			callback: (e) => post(`/transport/${e.options.bus}/${e.options.action}`),
		},
		volume_set: {
			name: 'Master volume: set',
			options: [{ type: 'number', id: 'value', label: 'Volume (0–100)', default: 100, min: 0, max: 100 }],
			callback: (e) => post(`/volume/${Math.min(100, Math.max(0, Number(e.options.value) || 0))}`),
		},
		volume_adjust: {
			name: 'Master volume: up/down',
			options: [
				{
					type: 'dropdown',
					id: 'direction',
					label: 'Direction',
					default: 'up',
					choices: [
						{ id: 'up', label: 'Up' },
						{ id: 'down', label: 'Down' },
					],
				},
				{ type: 'number', id: 'step', label: 'Step (0–100)', default: 5, min: 1, max: 100 },
			],
			callback: (e) => post(`/volume/${e.options.direction === 'down' ? 'down' : 'up'}/${Math.max(1, Number(e.options.step) || 5)}`),
		},
	}
}
