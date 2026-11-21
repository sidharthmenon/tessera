import { test } from 'node:test'
import assert from 'node:assert/strict'
import { apiUrl, eventsUrl } from './api.js'
import { clock, cueShape, cueVariable, engineConnected, inPreview, onProgram, previewEdited, variableDefinitions, variableValues } from './state.js'
import type { Snapshot } from './types.js'

const state: Snapshot = {
	show: 'gala.tshow',
	engine: 'connected',
	groups: [
		{ number: 1, name: 'Main', cues: [{ number: 1, name: 'Walk-in', hotkey: null }, { number: 5, name: 'Keynote', hotkey: 'K' }] },
		{ number: 2, name: 'Stingers', cues: [{ number: 1, name: 'Logo', hotkey: null }] },
	],
	preview: { group: 1, cue: 5, name: 'Keynote', empty: false, edited: true },
	program: { group: 1, cue: 1, name: 'Walk-in', empty: false, edited: false },
	volume: 80,
	time: { elapsedMs: 65_400, remainingMs: 3_725_000, clip: 'loop.mov' },
}

test('clock', () => {
	assert.deepEqual([clock(0), clock(65_400), clock(3_725_000), clock(null)], ['0:00', '1:05', '1:02:05', ''])
})

test('variables: fixed ones and one per cue', () => {
	const ids = variableDefinitions(state).map((v) => v.variableId)
	assert.ok(ids.includes('pgm_remaining') && ids.includes('cue_1_5') && ids.includes('cue_2_1'))
	assert.equal(ids.length, 12 + 3)
	assert.equal(variableDefinitions(null).length, 12)
	const v = variableValues(state)
	assert.equal(v.pvw_name, 'Keynote')
	assert.equal(v.pgm_cue, 1)
	assert.equal(v.pgm_remaining, '1:02:05')
	assert.equal(v[cueVariable(2, 1)], 'Logo')
})

test('variables: an unstored Preview and nothing on air', () => {
	const v = variableValues({ ...state, preview: { group: null, cue: null, name: null, empty: false, edited: false }, program: { group: null, cue: null, name: null, empty: true, edited: false }, time: null })
	assert.deepEqual([v.pvw_name, v.pgm_name, v.pgm_remaining], ['not stored', '', ''])
})

test('feedbacks', () => {
	assert.ok(inPreview(state, 1, 5) && !inPreview(state, 1, 1) && !inPreview(state, 2, 5))
	assert.ok(onProgram(state, 1, 1) && !onProgram(state, 2, 1))
	assert.ok(previewEdited(state) && engineConnected(state))
	assert.ok(!inPreview(null, 1, 1) && !engineConnected({ ...state, engine: 'stopped' }))
})

test('cue shape changes only with the cue list', () => {
	assert.equal(cueShape(state), cueShape({ ...state, preview: state.program, volume: 10 }))
	assert.notEqual(cueShape(state), cueShape({ ...state, groups: state.groups.slice(1) }))
})

test('urls', () => {
	const c = { host: '10.0.0.5', port: 7410, key: 'a b&c' }
	assert.equal(apiUrl(c), 'http://10.0.0.5:7410/api/v1')
	assert.equal(eventsUrl(c), 'ws://10.0.0.5:7410/api/v1/events?key=a%20b%26c')
})
