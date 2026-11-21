// Talks to Tessera's remote-control API: commands over HTTP, live state over a WebSocket that
// reconnects by itself. Uses Node's built-in fetch and WebSocket (Node 22).

import type { Config, Snapshot } from './types.js'

export type Status = 'connecting' | 'ok' | 'auth' | 'failed'

export interface Handlers {
	state(state: Snapshot): void
	status(status: Status, message?: string): void
}

/** `http://host:port/api/v1` */
export function apiUrl(c: Config): string {
	return `http://${c.host}:${c.port}/api/v1`
}

/** The events WebSocket, with the key in the query (browsers' WebSocket can't set headers). */
export function eventsUrl(c: Config): string {
	return `ws://${c.host}:${c.port}/api/v1/events?key=${encodeURIComponent(c.key)}`
}

/** Waits between reconnects, growing to the last. */
const BACKOFF_MS = [1000, 2000, 5000]

export class TesseraClient {
	private socket: WebSocket | null = null
	private timer: NodeJS.Timeout | null = null
	private attempt = 0
	private closed = false

	constructor(
		private readonly config: Config,
		private readonly handlers: Handlers,
	) {}

	/** Starts connecting (and keeps reconnecting until `destroy`). */
	connect(): void {
		void this.open()
	}

	destroy(): void {
		this.closed = true
		if (this.timer) clearTimeout(this.timer)
		this.socket?.close()
		this.socket = null
	}

	/** Runs a command (`path` after `/api/v1`, e.g. `/go`); throws with Tessera's message if refused. */
	async post(path: string): Promise<Snapshot> {
		const res = await fetch(`${apiUrl(this.config)}${path}`, { method: 'POST', headers: { 'X-Tessera-Key': this.config.key } })
		const body = (await res.json().catch(() => ({}))) as { error?: string } & Partial<Snapshot>
		if (!res.ok) throw new Error(body.error ?? `HTTP ${res.status}`)
		return body as Snapshot
	}

	private async open(): Promise<void> {
		if (this.closed) return
		this.handlers.status('connecting')
		// A plain request first: a WebSocket refused for a wrong key only says "closed".
		try {
			const res = await fetch(`${apiUrl(this.config)}/state`, { headers: { 'X-Tessera-Key': this.config.key } })
			if (res.status === 401) {
				this.handlers.status('auth', 'Wrong access key (Tessera: status bar → Remote)')
				return this.retry()
			}
			if (!res.ok) throw new Error(`HTTP ${res.status}`)
			this.handlers.state((await res.json()) as Snapshot)
		} catch (e) {
			this.handlers.status('failed', `Tessera not reachable at ${this.config.host}:${this.config.port} (${(e as Error).message}); is remote control on?`)
			return this.retry()
		}
		if (this.closed) return
		const socket = new WebSocket(eventsUrl(this.config))
		this.socket = socket
		socket.addEventListener('open', () => {
			this.attempt = 0
			this.handlers.status('ok')
		})
		socket.addEventListener('message', (event) => {
			try {
				const msg = JSON.parse(String(event.data)) as { type?: string; state?: Snapshot }
				if (msg.type === 'state' && msg.state) this.handlers.state(msg.state)
			} catch {
				// not ours; ignore
			}
		})
		socket.addEventListener('close', () => {
			if (this.socket !== socket) return
			this.socket = null
			if (!this.closed) {
				this.handlers.status('failed', 'Connection to Tessera lost; reconnecting')
				this.retry()
			}
		})
		socket.addEventListener('error', () => socket.close())
	}

	private retry(): void {
		if (this.closed) return
		const wait = BACKOFF_MS[Math.min(this.attempt, BACKOFF_MS.length - 1)]
		this.attempt++
		this.timer = setTimeout(() => void this.open(), wait)
	}
}
