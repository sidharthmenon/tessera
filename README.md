# Tessera

A media server for live LED and event shows, and its Bitfocus Companion connection.

| Folder | What it is |
|---|---|
| [`tessera/`](tessera/) | The media server: render engine (Rust, wgpu, FFmpeg) and controller app (Tauri + SvelteKit). Build, run and show notes are in [`tessera/README.md`](tessera/README.md). |
| [`companion-module-tessera/`](companion-module-tessera/) | A [Bitfocus Companion](https://bitfocus.io/companion) connection module that controls Tessera from a Stream Deck (GO, cues, playback, volume, live feedback). |

External control (the HTTP/WebSocket API both use): [`tessera/docs/external-control.md`](tessera/docs/external-control.md).

## Quick start

```bash
cd tessera/app
npm install
npm run tauri dev
```

Then, for Companion: in Tessera, status bar → **Remote** → *Enable remote control*, and follow
[`companion-module-tessera/README.md`](companion-module-tessera/README.md).
