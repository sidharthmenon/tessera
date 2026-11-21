# Tessera

A media server for live LED and event shows, and its Bitfocus Companion connection.

| Folder | What it is |
|---|---|
| [`tessera/`](tessera/) | The media server: render engine (Rust, wgpu, FFmpeg) and controller app (Tauri + SvelteKit). Build, run and show notes are in [`tessera/README.md`](tessera/README.md). |
| [`companion-module-tessera/`](companion-module-tessera/) | A [Bitfocus Companion](https://bitfocus.io/companion) connection module that controls Tessera from a Stream Deck (GO, cues, playback, volume, live feedback). |

External control (the HTTP/WebSocket API both use): [`tessera/docs/external-control.md`](tessera/docs/external-control.md).

## Download

Installers are on the [Releases](../../releases) page: macOS (Apple Silicon, macOS 12+) `.dmg`,
Windows x64 `setup.exe` (untested so far), and the Companion module `.tgz`. They aren't signed;
the release notes say how to open them.

## Build from source

```bash
cd tessera/app
npm install
npm run tauri dev
```

Then, for Companion: in Tessera, status bar → **Remote** → *Enable remote control*, and follow
[`companion-module-tessera/README.md`](companion-module-tessera/README.md).

## License

- `tessera/`: [GPL-3.0](tessera/LICENSE).
- `companion-module-tessera/`: [MIT](companion-module-tessera/LICENSE).
- The installers include [FFmpeg](https://ffmpeg.org) built under the LGPL 2.1+, dynamically
  linked. Each release says which FFmpeg version and source it uses.
