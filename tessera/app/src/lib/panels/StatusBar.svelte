<script lang="ts">
  import { formatUsage } from '$lib/logic';
  import { newRemoteKey, restartEngine, setRemote, startEngine, stopEngine, toast, ui } from '$lib/store.svelte';

  let menu = $state<{ x: number; y: number } | null>(null);
  let remoteMenu = $state<{ x: number; y: number } | null>(null);
  const remote = $derived(ui.remote);
  function openRemote(e: MouseEvent) {
    e.stopPropagation();
    remoteMenu = remoteMenu ? null : { x: e.clientX, y: e.clientY };
  }
  async function copyKey() {
    if (!remote?.key) return;
    await navigator.clipboard.writeText(remote.key);
    toast('Access key copied');
  }
  function openMenu(e: MouseEvent) {
    e.stopPropagation();
    menu = menu ? null : { x: e.clientX, y: e.clientY };
  }

  const text = $derived(
    {
      connected: ui.engine?.adopted ? 'engine connected (adopted: edits to the live cue reach air on the next GO)' : 'engine connected',
      held: 'engine held: kept from an earlier session, outputs untouched until you open or create a show',
      restarting: ui.restarting || 'engine restarting',
      connecting: 'engine connecting…',
      stopped: 'engine stopped: outputs are black (Engine → Start)',
    }[ui.status],
  );
  const bound = $derived(ui.outputs.filter((o) => o.bound).length);
</script>

<svelte:window
  onclick={() => (menu = remoteMenu = null)}
  onkeydown={(e) => {
    if (e.key === 'Escape') menu = remoteMenu = null;
  }}
/>

<footer class="status">
  <button class="engine-menu" onclick={openMenu} title="Start, stop or restart the render engine">Engine ▴</button>
  <span class="engine {ui.status}"><span class="dot"></span>{text}</span>
  <button class="remote-menu" class:on={remote?.running} class:bad={remote?.enabled && !remote?.running} onclick={openRemote} title="Remote control for Bitfocus Companion and other systems">
    {#if remote?.running}Remote · {remote.port}{remote.clients ? ` · ${remote.clients} connected` : ''}{:else if remote?.enabled}Remote: error{:else}Remote off{/if}
  </button>
  {#if ui.stats}
    <span>{ui.stats.fps.toFixed(1)} fps</span>
    <span class:bad={ui.stats.dropped > 0}>{ui.stats.dropped} dropped</span>
  {/if}
  {#if ui.usage}
    {@const u = formatUsage(ui.usage)}
    <span class="usage" title={u.detail}>{u.tessera}</span>
    <span class="usage" class:bad={u.tight}>{u.machine}</span>
  {/if}
  {#if ui.outputs.length}<span>outputs {bound}/{ui.outputs.length} bound</span>{/if}
  {#if ui.notice}<span class="notice">{ui.notice}</span>{/if}
  {#if ui.error}
    <button class="error" title="Dismiss" onclick={() => (ui.error = '')}>⚠ {ui.error}</button>
  {/if}
</footer>

{#if menu}
  <div class="menu up" style:left="{menu.x}px" style:top="{menu.y - 6}px">
    {#if ui.engineStopped}
      <button onclick={() => startEngine()}>Start engine</button>
    {:else}
      <button onclick={() => restartEngine()} title="Replace the engine; the show, Preview and Program come back">Restart engine</button>
      <button onclick={() => stopEngine()} title="Every output goes black until Start">Stop engine…</button>
    {/if}
  </div>
{/if}

{#if remoteMenu && remote}
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div class="menu up remote" style:left="{remoteMenu.x}px" style:top="{remoteMenu.y - 6}px" onclick={(e) => e.stopPropagation()}>
    <label class="check"><input type="checkbox" checked={remote.enabled} onchange={(e) => setRemote(e.currentTarget.checked, remote.port)} /> Remote control (Companion)</label>
    <label class="field">
      <span>Port</span>
      <input
        type="number"
        min="1024"
        max="65535"
        value={remote.port}
        onchange={(e) => {
          const port = e.currentTarget.valueAsNumber;
          if (Number.isInteger(port) && port >= 1024 && port <= 65535) setRemote(remote.enabled, port);
          else e.currentTarget.value = String(remote.port);
        }}
      />
    </label>
    {#if remote.enabled}
      <div class="field"><span>Key</span><code title={remote.key}>{remote.key.slice(0, 8)}…</code><button onclick={copyKey}>Copy</button><button onclick={() => newRemoteKey()}>New</button></div>
      {#if remote.running}
        <p class="hint">In Companion: host <b>{remote.address ?? 'this computer’s IP'}</b>, port <b>{remote.port}</b>, and this key. Same computer: 127.0.0.1.</p>
      {/if}
    {/if}
    {#if remote.error}<p class="err">{remote.error}</p>{/if}
  </div>
{/if}

<style>
  .remote-menu {
    padding: 0 6px;
    font-size: 11px;
  }
  .remote-menu.on {
    border-color: var(--pvw);
    color: var(--pvw);
  }
  .remote-menu.bad {
    border-color: var(--live);
    color: #ffb4ae;
  }
  .remote {
    min-width: 280px;
    gap: 6px;
    padding: 8px 10px;
    font-size: 12px;
  }
  .remote .check,
  .remote .field {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .remote .field span {
    width: 34px;
    color: var(--muted);
  }
  .remote .field input {
    width: 90px;
  }
  .remote .field button {
    background: var(--panel);
    border: 1px solid var(--border);
    padding: 1px 8px;
  }
  .remote code {
    font-size: 11px;
  }
  .remote .hint {
    margin: 0;
    color: var(--muted);
    white-space: normal;
  }
  .remote .err {
    margin: 0;
    color: #ffb4ae;
    white-space: normal;
  }
  .engine-menu {
    padding: 0 6px;
    font-size: 11px;
  }
  .status {
    grid-area: status;
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 3px 10px;
    background: var(--panel);
    border-top: 1px solid var(--border);
    color: var(--muted);
    font-size: 11px;
    white-space: nowrap;
    min-width: 0;
  }
  .engine {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--muted);
  }
  .connected .dot {
    background: var(--pvw);
  }
  .held .dot {
    background: var(--warn);
  }
  .held,
  .restarting,
  .stopped {
    color: var(--warn);
  }
  .stopped .dot {
    background: var(--live);
  }
  .restarting .dot {
    background: var(--live);
  }
  .bad {
    color: var(--warn);
  }
  .usage {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    flex-shrink: 100;
    font-variant-numeric: tabular-nums;
  }
  .notice {
    color: var(--accent-2);
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .error {
    margin-left: auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    background: rgba(239, 59, 50, 0.18);
    border-color: var(--live);
    color: #ffb4ae;
    padding: 0 8px;
  }
</style>
