import { combineRgb, type CompanionButtonPresetDefinition, type CompanionPresetDefinitions } from '@companion-module/base'
import { BLACK, PGM, PVW, WHITE } from './feedbacks.js'
import { cueVariable } from './state.js'
import type { Snapshot } from './types.js'

const GREY = combineRgb(40, 40, 40)

function button(
	category: string,
	name: string,
	text: string,
	actionId: string,
	options: Record<string, string | number> = {},
	extra: Partial<CompanionButtonPresetDefinition> = {},
): CompanionButtonPresetDefinition {
	return {
		type: 'button',
		category,
		name,
		style: { text, size: 'auto', color: WHITE, bgcolor: BLACK },
		steps: [{ down: [{ actionId, options }], up: [] }],
		feedbacks: [],
		...extra,
	}
}

/** Fixed buttons, plus one per cue of each group (loads it; green in Preview, red on Program). */
export function presets(state: Snapshot | null): CompanionPresetDefinitions {
	const out: CompanionPresetDefinitions = {
		go: button('Show', 'GO', 'GO', 'go', {}, { style: { text: 'GO', size: '24', color: BLACK, bgcolor: combineRgb(255, 138, 31) } }),
		next: button('Show', 'Next cue', 'NEXT\\n$(tessera:pvw_name)', 'next'),
		previous: button('Show', 'Previous cue', 'PREV', 'previous'),
		on_air: button('Show', 'On air: cue and time left', '$(tessera:pgm_name)\\n$(tessera:pgm_remaining)', 'go', {}, {
			style: { text: '$(tessera:pgm_name)\\n$(tessera:pgm_remaining)', size: 'auto', color: WHITE, bgcolor: PGM },
			steps: [],
		}),
		pgm_play: button('Playback', 'Program: play', 'PGM\\nPLAY', 'transport', { bus: 'program', action: 'play' }),
		pgm_pause: button('Playback', 'Program: pause', 'PGM\\nPAUSE', 'transport', { bus: 'program', action: 'pause' }),
		pgm_stop: button('Playback', 'Program: stop', 'PGM\\nSTOP', 'transport', { bus: 'program', action: 'stop' }),
		pgm_restart: button('Playback', 'Program: restart', 'PGM\\nRESTART', 'transport', { bus: 'program', action: 'restart' }),
		pvw_play: button('Playback', 'Preview: play', 'PVW\\nPLAY', 'transport', { bus: 'preview', action: 'play' }),
		pvw_pause: button('Playback', 'Preview: pause', 'PVW\\nPAUSE', 'transport', { bus: 'preview', action: 'pause' }),
		vol_up: button('Volume', 'Volume up 5', 'VOL +\\n$(tessera:volume)', 'volume_adjust', { direction: 'up', step: 5 }),
		vol_down: button('Volume', 'Volume down 5', 'VOL −\\n$(tessera:volume)', 'volume_adjust', { direction: 'down', step: 5 }),
	}
	for (const g of state?.groups ?? []) {
		for (const c of g.cues) {
			const options = { group: g.number, cue: c.number }
			out[`cue_${g.number}_${c.number}`] = button(`Cues: ${g.name}`, `Cue ${c.number}: ${c.name}`, `${c.number}\\n$(tessera:${cueVariable(g.number, c.number)})`, 'load_cue', options, {
				style: { text: `${c.number}\\n$(tessera:${cueVariable(g.number, c.number)})`, size: 'auto', color: WHITE, bgcolor: GREY },
				feedbacks: [
					{ feedbackId: 'cue_in_preview', options, style: { bgcolor: PVW, color: WHITE } },
					{ feedbackId: 'cue_on_program', options, style: { bgcolor: PGM, color: WHITE } },
				],
			})
		}
	}
	return out
}
