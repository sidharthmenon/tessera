# External control

Tessera can be driven from outside the app: from a Stream Deck through
[Bitfocus Companion](https://bitfocus.io/companion), or from any script, show-control system or
other app that can send HTTP requests. It's **off** until you turn it on.

- **Companion users:** install the Tessera module ([`companion-module-tessera`](../../companion-module-tessera/README.md))
  and skip to [Turning it on](#turning-it-on). The module does everything below for you.
- **Everyone else:** this page is the full reference.

## Turning it on

1. Click **Remote** in Tessera's status bar (bottom left, next to **Engine**).
2. Tick **Remote control (Companion)**. Tessera starts listening on the network and makes an access
   key the first time. The button turns green: `Remote · 7410`, plus `· 1 connected` while
   Companion (or another client) is connected.
3. The menu shows what to type into the other system:
   - **host**: this computer's address on the network (or `127.0.0.1` on the same computer);
   - **port**: 7410 unless you change it (any free port from 1024 to 65535);
   - **access key**: press **Copy**.

The first time, macOS may ask whether Tessera may accept incoming network connections: allow it,
or nothing on the network can reach it. On Windows, allow it in the Windows Defender Firewall
prompt.

The setting, port and key are remembered (in `remote.json` in Tessera's settings folder), so remote
control comes back on by itself the next time Tessera starts, if it was on.

**New** makes a new key. The old one stops working at once, so give connected systems the new one.
Untick the box to turn remote control off: the port closes.

**Security.** Anyone who has the key and can reach the port can GO, so keep the key to the show
network. Tessera only listens while remote control is on, and every request without the right
key is refused. The connection is plain HTTP (no encryption), which is fine on a closed show
network, but don't expose the port to the internet.

## The API

Base URL: `http://<host>:<port>/api/v1`. Every request carries the key, as a header
`X-Tessera-Key: <key>` or as a query parameter `?key=<key>` (handy for tools that can't set headers,
and needed for the WebSocket from a browser).

Groups and cues are numbered **from 1, as on screen**: group 1 is the first tab of the cue grid,
and cue 3 is the tile labelled 3.

Every command answers with the **state** after it ran (see [State](#state)), or with an error.

| Method and path | Does |
|---|---|
| `GET /state` | The current state |
| `POST /go` | GO: Preview to Program, with Preview's transition |
| `POST /cue/{group}/{cue}/load` | Load the cue into Preview |
| `POST /cue/{group}/{cue}/fire` | Load the cue and GO (one press) |
| `POST /next` | Load the next cue (after Preview's cue) |
| `POST /previous` | Load the previous cue |
| `POST /transport/{bus}/{action}` | `bus`: `program` or `preview`. `action`: `play`, `pause`, `stop` (back to the first frame), `restart`. Acts on every clip on that bus. |
| `POST /volume/{value}` | Master volume, 0–100 |
| `POST /volume/up/{step}`, `POST /volume/down/{step}` | Master volume up/down by `step` (0.1–100) |
| `GET /events` | WebSocket: the state, live (see [Live state](#live-state)) |

Commands from outside never ask for confirmation, unlike the app: **GO**, **fire**, and
**stop**/**restart** on Program act at once.

### Examples

```bash
KEY=paste-the-key-here
T=http://192.168.1.20:7410/api/v1
```

```bash
curl -H "X-Tessera-Key: $KEY" $T/state
```

```bash
curl -X POST -H "X-Tessera-Key: $KEY" $T/cue/1/3/load
```

```bash
curl -X POST -H "X-Tessera-Key: $KEY" $T/go
```

```bash
curl -X POST "$T/transport/program/pause?key=$KEY"
```

```bash
curl -X POST "$T/volume/down/5?key=$KEY"
```

### Errors

Errors come back as `{ "error": "…" }` with a status:

| Status | When |
|---|---|
| 401 | Missing or wrong key (or remote control just got a new key) |
| 400 | A bad number or name in the path, a group or cue that doesn't exist (`"group 1 (Main) has no cue 7"`) |
| 409 | GO with nothing in Preview |
| 503 | Playback while the engine isn't connected |

### State

```json
{
  "show": "gala.tshow",
  "engine": "connected",
  "groups": [
    { "number": 1, "name": "Main", "cues": [
      { "number": 1, "name": "Walk-in", "hotkey": null },
      { "number": 3, "name": "Opener", "hotkey": "O" }
    ] }
  ],
  "preview": { "group": 1, "cue": 3, "name": "Opener", "empty": false, "edited": false },
  "program": { "group": 1, "cue": 1, "name": "Walk-in", "empty": false, "edited": false },
  "volume": 80,
  "time": { "elapsedMs": 65400, "remainingMs": 120000, "clip": "walkin-loop.mov" }
}
```

| Field | Meaning |
|---|---|
| `show` | The show file's name, or `Untitled show` |
| `engine` | `connected`, `held` (kept from an earlier session, waiting for a show), `connecting` (starting or restarting), `stopped` (stopped from the Engine menu) |
| `groups` | The cue grid: each tab with its cues (only slots that hold a cue) |
| `preview` | The cue Preview came from (`group`/`cue`/`name` are null if Preview isn't from a cue), `empty` if Preview shows nothing, `edited` if it changed since that cue was loaded |
| `program` | The cue last taken to Program, `empty` if nothing is on air |
| `volume` | Master volume, 0–100 |
| `time` | The Program clip that ends last (the visible video with the most time left, counting its trim and speed): elapsed and remaining in real milliseconds, and its name. `null` when no clip plays on Program. |

### Live state

Connect a WebSocket to `ws://<host>:<port>/api/v1/events?key=<key>`. Tessera sends the state at
once, then again whenever it changes (a cue loaded, a GO, a volume change, the engine restarting, a
cue renamed, …), each second while `time` counts, and at least every 5 s even when nothing changed:
a client that hears nothing for much longer (the Companion module waits 15 s) can take the
connection as dead and reconnect. Every message is:

```json
{ "type": "state", "state": { …the state above… } }
```

Nothing needs to be sent back. A wrong key is refused before the connection opens. If the
connection drops (Tessera restarting, remote control turned off), reconnect with a back-off;
the first message after reconnecting is the full state.

## Companion without the Tessera module

Companion's **Generic HTTP** module can fire commands too, without feedback or variables:
add a connection with base URL `http://<host>:7410/api/v1` and, in its settings, the header
`X-Tessera-Key: <key>` (or append `?key=<key>` to each URL). Then use **POST** actions with paths
such as `/go`, `/cue/1/3/load` or `/transport/program/pause`.

## Trying it without a show

`cargo run --example remote_demo` (in `tessera/app/src-tauri`) serves the same API on
`127.0.0.1:7411` with key `demo`, over a small demo show and with no engine or window. It's what
the Companion module's `npm run smoke` test uses. Options: `--port`, `--key`, `--host`.
