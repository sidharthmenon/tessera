import { combineRgb, type CompanionFeedbackDefinitions, type CompanionInputFieldNumber } from '@companion-module/base'
import type { TesseraInstance } from './main.js'
import { engineConnected, inPreview, onProgram, previewEdited } from './state.js'

/** Tessera's own colours: Preview green, Program red. */
export const PVW = combineRgb(26, 140, 80)
export const PGM = combineRgb(200, 40, 34)
export const WHITE = combineRgb(255, 255, 255)
export const BLACK = combineRgb(0, 0, 0)

const groupField: CompanionInputFieldNumber = { type: 'number', id: 'group', label: 'Group (1 = first tab)', default: 1, min: 1, max: 99 }
const cueField: CompanionInputFieldNumber = { type: 'number', id: 'cue', label: 'Cue number', default: 1, min: 1, max: 999 }

export function feedbacks(self: TesseraInstance): CompanionFeedbackDefinitions {
	const n = (v: unknown) => Math.round(Number(v) || 0)
	return {
		cue_in_preview: {
			type: 'boolean',
			name: 'Cue is in Preview',
			defaultStyle: { bgcolor: PVW, color: WHITE },
			options: [groupField, cueField],
			callback: (f) => inPreview(self.state, n(f.options.group), n(f.options.cue)),
		},
		cue_on_program: {
			type: 'boolean',
			name: 'Cue is on Program (on air)',
			defaultStyle: { bgcolor: PGM, color: WHITE },
			options: [groupField, cueField],
			callback: (f) => onProgram(self.state, n(f.options.group), n(f.options.cue)),
		},
		preview_edited: {
			type: 'boolean',
			name: 'Preview was edited (differs from its cue)',
			defaultStyle: { bgcolor: combineRgb(255, 138, 31), color: BLACK },
			options: [],
			callback: () => previewEdited(self.state),
		},
		engine_connected: {
			type: 'boolean',
			name: 'Engine connected',
			defaultStyle: { bgcolor: PVW, color: WHITE },
			options: [],
			callback: () => engineConnected(self.state),
		},
	}
}
