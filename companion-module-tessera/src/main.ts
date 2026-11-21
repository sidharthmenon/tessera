import { InstanceBase, InstanceStatus, Regex, runEntrypoint, type SomeCompanionConfigField } from '@companion-module/base'
import { actions } from './actions.js'
import { TesseraClient, type Status } from './api.js'
import { feedbacks } from './feedbacks.js'
import { presets } from './presets.js'
import { cueShape, variableDefinitions, variableValues } from './state.js'
import { DEFAULT_PORT, type Config, type Snapshot } from './types.js'

export class TesseraInstance extends InstanceBase<Config> {
	config: Config = { host: '127.0.0.1', port: DEFAULT_PORT, key: '' }
	client: TesseraClient | null = null
	state: Snapshot | null = null
	/** The groups/cues the variable and preset definitions were made for. */
	private shape = ''

	async init(config: Config): Promise<void> {
		this.config = config
		this.setActionDefinitions(actions(this))
		this.setFeedbackDefinitions(feedbacks(this))
		this.setVariableDefinitions(variableDefinitions(null))
		this.setPresetDefinitions(presets(null))
		this.start()
	}

	async destroy(): Promise<void> {
		this.client?.destroy()
		this.client = null
	}

	async configUpdated(config: Config): Promise<void> {
		this.config = config
		this.client?.destroy()
		this.start()
	}

	getConfigFields(): SomeCompanionConfigField[] {
		return [
			{
				type: 'static-text',
				id: 'info',
				label: 'Tessera',
				value: 'In Tessera: status bar → Remote → turn on remote control. It shows this computer’s address, the port and the access key.',
				width: 12,
			},
			{ type: 'textinput', id: 'host', label: 'Host (Tessera’s computer; 127.0.0.1 if the same)', width: 8, default: '127.0.0.1', regex: Regex.HOSTNAME },
			{ type: 'number', id: 'port', label: 'Port', width: 4, default: DEFAULT_PORT, min: 1, max: 65535 },
			{ type: 'textinput', id: 'key', label: 'Access key (from Tessera’s Remote menu)', width: 12, default: '' },
		]
	}

	private start(): void {
		if (!this.config.host || !this.config.key) {
			this.updateStatus(InstanceStatus.BadConfig, 'Set the host and the access key')
			return
		}
		this.client = new TesseraClient(this.config, {
			state: (s) => this.onState(s),
			status: (s, message) => this.onStatus(s, message),
		})
		this.client.connect()
	}

	private onStatus(status: Status, message?: string): void {
		const map = { connecting: InstanceStatus.Connecting, ok: InstanceStatus.Ok, auth: InstanceStatus.AuthenticationFailure, failed: InstanceStatus.ConnectionFailure }
		this.updateStatus(map[status], message ?? null)
	}

	private onState(state: Snapshot): void {
		this.state = state
		const shape = cueShape(state)
		if (shape !== this.shape) {
			this.shape = shape
			this.setVariableDefinitions(variableDefinitions(state))
			this.setPresetDefinitions(presets(state))
		}
		this.setVariableValues(variableValues(state))
		this.checkFeedbacks()
	}
}

runEntrypoint(TesseraInstance, [])
