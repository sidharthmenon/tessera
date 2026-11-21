// End-to-end check of the module's client against a running Tessera remote API (not part of the
// module). Start the demo server first:
//   cd tessera/app/src-tauri && cargo run --example remote_demo
// then: npm run smoke   (TESSERA_HOST / TESSERA_PORT / TESSERA_KEY override 127.0.0.1:7411, "demo")

import assert from 'node:assert/strict'
import { TesseraClient, type Status } from './api.js'
import type { Snapshot } from './types.js'

const config = { host: process.env.TESSERA_HOST ?? '127.0.0.1', port: Number(process.env.TESSERA_PORT ?? 7411), key: process.env.TESSERA_KEY ?? 'demo' }

async function main() {
	let latest: Snapshot | null = null
	const statuses: Status[] = []
	const waiters: Array<() => void> = []
	const client = new TesseraClient(config, {
		state: (s) => {
			latest = s
			waiters.splice(0).forEach((w) => w())
		},
		status: (s) => statuses.push(s),
	})
	/** Resolves once a pushed state satisfies `ok` (within 3 s). */
	const until = (ok: (s: Snapshot) => boolean, what: string) =>
		new Promise<Snapshot>((resolve, reject) => {
			const timer = setTimeout(() => reject(new Error(`timed out waiting for ${what}`)), 3000)
			const check = () => (latest && ok(latest) ? (clearTimeout(timer), resolve(latest)) : waiters.push(check))
			check()
		})
	client.connect()
	const first = await until((s) => s.groups.length > 0, 'the first state')
	await until(() => statuses.includes('ok'), 'the WebSocket')
	console.log(`connected: ${first.show}, groups ${first.groups.map((g) => `${g.number} ${g.name} (${g.cues.length} cues)`).join(', ')}`)

	const [g, c] = [first.groups[0].number, first.groups[0].cues[1].number]
	await client.post(`/cue/${g}/${c}/load`)
	await until((s) => s.preview.cue === c, 'the loaded cue in Preview (pushed)')
	console.log(`load ${g}/${c}: Preview pushed`)
	await client.post('/go')
	const taken = await until((s) => s.program.cue === c, 'GO (pushed)')
	console.log(`GO: Program now ${taken.program.name}`)
	await client.post('/next')
	await until((s) => s.preview.cue !== c, 'next cue (pushed)')
	await client.post('/volume/40')
	await until((s) => s.volume === 40, 'volume 40 (pushed)')
	await assert.rejects(client.post('/cue/1/999/load'), /no cue 999/)
	console.log('next, volume and a refused cue: ok')

	const wrong = new TesseraClient({ ...config, key: 'wrong' }, { state: () => {}, status: (s) => s === 'auth' && (console.log('wrong key: AuthenticationFailure'), wrong.destroy()) })
	wrong.connect()
	await new Promise((r) => setTimeout(r, 500))
	wrong.destroy()
	client.destroy()
	console.log('smoke: all passed')
}

main().catch((e) => {
	console.error(`smoke failed: ${e.message}`)
	process.exit(1)
})
