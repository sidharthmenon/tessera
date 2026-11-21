# companion-module-tessera

A [Bitfocus Companion](https://bitfocus.io/companion) connection module for the Tessera media
server ([`../tessera`](../tessera)): GO, cues, playback and master volume from a Stream Deck, with
live cue and on-air feedback.

- Companion **4.0 or newer** (also 5.x): built on `@companion-module/base` 1.12, Node 22 runtime.
- Talks to Tessera's remote-control API ([`../tessera/docs/external-control.md`](../tessera/docs/external-control.md)):
  HTTP for commands, a WebSocket for live state. No other dependencies.
- What the user sees in Companion (actions, feedbacks, variables, presets): [`companion/HELP.md`](companion/HELP.md).

## Use it in Companion

Either import the package:

```bash
npm install
npm run package
```

then in Companion: **Modules** → **Import module package** → `tessera-0.1.0.tgz`.

Or load it as a developer module (picks up rebuilds): put this folder inside a folder of its own
(e.g. `~/companion-dev/companion-module-tessera`), run `npm install && npm run build` here, then
in the Companion launcher: cog (Advanced settings) → **Developer** → select `~/companion-dev` and
turn on **Enable Developer Modules**. `npm run dev` rebuilds on every change.

Then add a **Tessera** connection and set host, port and access key from Tessera's status bar →
**Remote** menu.

## Develop

| Command | Does |
|---|---|
| `npm run build` | TypeScript → `dist/` |
| `npm run dev` | Rebuild on change |
| `npm test` | Unit tests (state → variables/feedbacks, URLs) |
| `npm run smoke` | End-to-end check of the client against a running API (see below) |
| `npm run package` | `tessera-<version>.tgz` for Companion |

The smoke test needs a server. Without running Tessera, use the demo server, which serves the real
API over a small demo show:

```bash
cd ../tessera/app/src-tauri && cargo run --example remote_demo   # 127.0.0.1:7411, key "demo"
```

```bash
npm run smoke
```

Against a real Tessera: `TESSERA_HOST=… TESSERA_PORT=7410 TESSERA_KEY=… npm run smoke` (it loads
cues, takes one to Program and sets the volume, so not during a show).

Source: `src/main.ts` (the instance), `src/api.ts` (HTTP + WebSocket client), `src/state.ts` (pure
mapping, tested), `src/actions.ts`, `src/feedbacks.ts`, `src/presets.ts`.
