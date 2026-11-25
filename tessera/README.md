# Tessera

Tessera is a media server for live LED and event shows. A headless render engine
(`crates/engine`, Rust + wgpu + FFmpeg with VideoToolbox decode) drives fullscreen outputs on
the connected displays. A Tauri 2 + SvelteKit controller (`app/`) handles screen management
(outputs, logical screens, mappings), a cue grid of layered cues and PVW → PGM Take with
Cut/Fade/Wipe transitions, and shows MJPEG previews. The engine runs as a separate process
(sidecar `tessera-render`), so a UI crash can't black out the screens. Phase 1 targets macOS.
Windows is untested.

## Prerequisites

- Rust stable, Node 20+
- macOS: `brew install ffmpeg pkgconf` (built against Homebrew FFmpeg 9)
- Windows (untested): the BtbN FFmpeg `lgpl-shared` build, with `FFMPEG_DIR` pointing at it

## Build, run, test

```sh
sh scripts/build-engine.sh        # once on a fresh checkout, before cargo build/test --workspace
cd app && npm install && npm run tauri dev   # dev: rebuilds the engine, then starts the app
```

Without that first step, `app/src-tauri/build.rs` puts a placeholder sidecar in
`app/src-tauri/binaries/`. The placeholder only prints an error.

Tests:

```sh
cargo test --workspace
cd app && npm run check && npm test
```

## Releasing

GitHub Actions builds the installers (`.github/workflows/release.yml` at the repo root):

1. Bump the version in `app/src-tauri/tauri.conf.json`, `app/package.json` and
   `../companion-module-tessera/package.json` (the workflow refuses a tag that doesn't match).
2. Commit, then `git tag vX.Y.Z && git push origin main vX.Y.Z`.
3. The workflow drafts a release with the macOS `.dmg`, the Windows `setup.exe`, the Companion
   module `.tgz` and the FFmpeg source. Check it on GitHub, then click **Publish**.

Actions → Release → **Run workflow** builds the same files (as run artifacts) without a release.

The installers bundle an **LGPL** FFmpeg, so they can be shipped with Tessera's GPL-3.0 code and
without Homebrew's GPL build:

- macOS: `scripts/build-ffmpeg-macos.sh <prefix>` builds FFmpeg 9.0.2 from source for macOS 12+
  (cached in CI). The app then needs macOS 12, Apple Silicon. Not notarized: see the release notes.
- Windows: BtbN's `lgpl-shared` build. `app/src-tauri/tauri.release-windows.conf.json` puts its
  DLLs next to `tessera-render.exe`. Not signed.

### Release build on this Mac

```sh
cd app && npm run release
```

This makes `target/release/bundle/macos/Tessera.app` (under `$CARGO_TARGET_DIR` if set):
`tauri build --bundles app`, then `scripts/bundle-ffmpeg-macos.sh` copies every non-system dylib
the sidecar loads into `Contents/Frameworks`, points the load paths there and ad-hoc signs the app.
With Homebrew's FFmpeg that app needs this Mac's macOS and is GPL. For what CI ships, build the LGPL
FFmpeg first:

```sh
sh scripts/build-ffmpeg-macos.sh ~/ffmpeg-lgpl
cd app && MACOSX_DEPLOYMENT_TARGET=12.0 PKG_CONFIG_PATH=~/ffmpeg-lgpl/lib/pkgconfig npm run release
```

To see the controller and `[engine]` logs, start the app from a terminal:
`Tessera.app/Contents/MacOS/tessera-app`. If macOS blocks a copied app, run
`xattr -dr com.apple.quarantine Tessera.app`.

The engine also keeps its own log, which survives a UI quit (an adopted engine has no terminal):
`logs/engine.log` in its cache folder. Started by the app, that is
`~/Library/Caches/dev.tessera.controller/logs/engine.log` (Windows:
`%LOCALAPPDATA%\dev.tessera.controller\logs\engine.log`); run on its own, `tessera` instead of
`dev.tessera.controller`. It is moved to `engine.log.1` past 8 MB. Every 10 s it logs a `perf:`
line (render-loop time per frame without the vsync wait, dropped frames), and each output's state changes (`hidden (occluded)`,
`stuck (no drawable)`, `showing`).

While it runs, the engine keeps the displays and the machine awake and opts out of App Nap and
Windows power throttling, so a show run only from Companion doesn't go black. Hardware-decoded
video is drawn straight from the decoder's buffers (macOS: VideoToolbox; Windows: D3D12VA on the
engine's DX12 device, the default backend there unless `WGPU_BACKEND` says otherwise);
`TESSERA_ZERO_COPY=0` in the engine's environment switches back to copying each frame (Windows:
D3D11VA), should a GPU or OS version draw it wrong.

## Shows, autosave, crash recovery

- **Show files.** A show is a `.tshow` file (JSON) saved wherever you choose. Media inside the
  show's folder is stored as a relative path. Every save is atomic (temp file + rename).
- **Autosave.** Tessera saves 2 s after the last edit (and on quit, and before opening or
  creating another show):
  - a show that has a file (saved once): **into that file**, so it always has the latest changes;
  - an untitled show: to `~/Library/Application Support/dev.tessera.controller/untitled.tshow.autosave`.
  - If the show's file can't be written (a disconnected drive), the changes go to
    `<show>.tshow.autosave` next to it, else to the untitled autosave, and the status bar says
    where; they are offered back as recovery.
- **Recovery.** Unsaved work from an earlier session is moved to `….autosave.prev`. The app then
  asks *Recover unsaved work?*. This happens at startup for the untitled autosave, and when you
  open a show whose autosave is newer than the show file. *Recover* opens the work as an
  untitled show; save it to keep it. *Not now* leaves the `.prev` file in place, and you can open
  it later with Open. An older `.prev` is never overwritten: later ones are kept as
  `….autosave.<time>.prev`, and the newest is the one offered.
- **Save As** gives the show a file (or a new one); from then on it saves into that file.
- **Screen setup.** The last outputs, screens and mappings are kept in `screens.tshow` in the
  same folder as the untitled autosave, and the app starts with them. Opening a show uses that
  show's setup instead (and keeps it for next time); **New show** keeps the current one.
- **Engine supervision.** If the engine dies or stops answering for 5 s, the controller kills it,
  starts a new one and re-sends the config, PVW, PGM and the master volume. What a restart
  restores:
  - Every clip on PGM starts again **from its first frame** (positions aren't restored).
  - Clips you paused or stopped come back paused or stopped, on their first frame.
  - A *Once* clip that had already ended stays off.
  - **Crash loop:** if the engine dies twice within 10 s of being restarted, the next restart
    leaves out every media layer (PGM keeps its patterns, PVW is emptied) and the status bar names
    what was left out. Take the cues again to bring them back. A minute without a crash resets
    this.
  - **By hand:** the **Engine** button at the left of the status bar restarts the engine (same
    restore as above; it asks first while something is on air) or stops it: every output goes
    black, and no engine runs until you choose **Start engine**. Neither counts as a crash.
- **UI force-quit.** The engine survives a force-quit or crash of the UI, and its outputs keep
  playing. On relaunch the controller adopts the engine through
  `~/Library/Application Support/dev.tessera.controller/engine.json` (Windows:
  `%APPDATA%\dev.tessera.controller\engine.json`; pid, port and token), so there is no blackout:
  - The status bar shows *engine held*, and the outputs stay untouched until you open or create
    a show.
  - After that, edits on the Program tab reach air on the next GO.
- **Quitting** (⌘Q, or closing the window). With nothing on air the app stops the engine and
  quits. With something on air (or an adopted engine) it asks:
  - *Keep outputs running and quit*: the screens stay as they are. The next launch adopts the
    engine, as after a force-quit.
  - *Stop outputs and quit*: every output goes black.
  - *Cancel*.

  A quit that can't ask (the Dock's Quit, logging out) keeps the outputs running if anything is on
  air.
- **One instance.** Launching Tessera again (including `npm run tauri dev` on the show Mac) only
  brings the running window to the front.

## Show-day checklist

Run this in the release app or in `npm run tauri dev`:

1. Add two **Window** outputs, plus one **Display** output on the second monitor.
2. Make one 3840×1080 screen and map half of it to each window.
3. Import an image and an mp4 (Import…, or drag the files from the Finder onto the window).
4. Build two cues. Preview is a working scene: drag media onto it, then click an empty cue slot
   to store it there (right-click a cue → *Update with current view* to overwrite one).
   - cue 1: looping background video + image;
   - cue 2: the same background video + a ProRes 4444 alpha overlay.
5. Load cue 2 to PVW. PGM doesn't change, and the overlay is paused on its first frame.
6. Press Space to fade. The background loop doesn't restart, the overlay starts on Take, and its
   alpha is correct.
7. Take a cue that touches only screen A. Screen B is untouched.
   - With the taken cue still in PVW, move a layer on the Preview tab. Program and the cue don't
     change; Preview shows *edited*. Live edits happen only on the Program tab.
8. 4× 4K layers hold 60 fps (engine fps in the status bar).
9. Audio is audible.
10. Kill the engine process. The controller restarts it and the outputs come back.
11. Force-quit the UI. The outputs keep playing. Relaunch: the status bar shows *engine held*.
    Then press ⌘Q while on air and choose *Keep outputs running and quit*: the outputs keep
    playing, and the next launch adopts the engine again.
12. Save, reopen: the show is identical.

On Windows, run the same checklist before the first show there.
[docs/windows-testing.md](docs/windows-testing.md) has the setup and the Windows-only checks.

## Not yet verified on real hardware

- [x] Unplugging and replugging a real monitor (Windows: the output closes once Windows drops
      the monitor, and reopens on replug)
- [x] A Display output on a second monitor, checked visually
- [x] Output windows actually presenting their halves, and Identify
- [ ] 4×4K at 60 fps in the release build with both previews watched. Windows, Intel UHD 630:
      one 4K60 layer runs at about 20 fps on the copy path, 60 fps with zero-copy
- [ ] A 4K ProRes 422 HQ clip at 60 fps. Make one with
      `ffmpeg -f lavfi -i testsrc2=s=3840x2160:r=60:d=10 -c:v prores_ks -profile:v 3 -pix_fmt yuv422p10le -color_range tv -colorspace bt709 -color_primaries bt709 -color_trc bt709 prores422hq.mov`
      (~650 MB, don't commit it). Import it, or put it in the spike clip folder and run
      `tessera-engine --demo-4k <folder>`, which adds it under the other four clips.
- [ ] App Nap: leave the engine with no output window visible for 10 minutes (UI in front,
      outputs covered or none). The status bar must never show *engine restarting*: the engine
      has to keep sending its 1 s stats.
- [x] Windows: D3D11VA decode and surface sizing
- [ ] Windows zero-copy, on NVIDIA, AMD and Intel: `cargo test -p tessera-engine` (runs
      `d3d12_frames_are_imported`), then a 4K H.264 and a 4K HEVC Main10 clip playing with no
      `zero-copy off` line in the engine log; once more with the D3D12 debug layer on (Graphics
      Tools optional feature; `d3d12_debug_layer_finds_no_errors`): no resource-state or fence
      errors. Intel UHD 630 on the legacy driver line: imports work and the debug layer finds
      nothing, but the driver crashes on decoder teardown, so it gets the copy path
      ([docs/windows-testing.md](docs/windows-testing.md)). NVIDIA and AMD not yet
- [ ] A USB capture device (Magewell/Elgato) as a live input: add, show, unplug and replug
- [x] Windows: DirectShow capture listing and opening
- [x] An NDI source (NDI Tools' Test Patterns) as a live input, with and without alpha (checked
      on Windows with another NDI sender)
- [ ] Menu bar and Dock behaviour while an output is in simple fullscreen

## Displays and audio

- **Main display.** A Display output on the main display (the one with the menu bar) stays at
  normal window level, so ⌘-Tab or the Dock icon brings the controller back in front. The menu bar
  may show over that output. On any other display an output sits above every window, the
  controller's included. Add Output marks the monitor the controller is on as *Controller
  display* and asks first.
- **Audio output.** The engine picks the macOS default output device when it starts. Set the
  default output (System Settings → Sound) before launching Tessera. After unplugging or
  replugging the audio interface, restart the engine: status bar → Engine → Restart engine. Its
  clips restart from their first frame (see Engine supervision).

## Live inputs

Capture devices (USB capture such as Magewell or Elgato, capture cards whose drivers present a
camera, webcams) appear under Resources → **Inputs**. **Add** puts one in Media as a live input;
drag it onto a screen like a clip. Play shows it live; Pause and Stop freeze it. If it stops
sending (unplugged), its layers go transparent, the status bar says so once, and it reconnects
by itself. Video only for now.

- **macOS** lists devices with AVFoundation and opens them through FFmpeg's avfoundation input
  (NV12, else UYVY). macOS asks for camera access the first time: allow it for Tessera (in dev
  runs, for the terminal that started `npm run tauri dev`). Turned off later? System Settings →
  Privacy & Security → Camera.
- **Windows** uses FFmpeg's DirectShow input: devices are listed by their stable device path.
  Allow desktop apps to use the camera (Settings → Privacy → Camera), and close any other app
  using the device: DirectShow devices are exclusive. Not yet tried on a Windows PC.
- **NDI® sources** show in the same list once the free NDI Tools (or NDI Runtime) is installed:
  Tessera loads the runtime when it starts looking (`$NDI_RUNTIME_DIR_V6`/`_V5`,
  `/usr/local/lib/libndi.dylib`, `/Library/NDI SDK for Apple/lib/macOS/libndi.dylib`; on Windows
  `Processing.NDI.Lib.x64.dll` from `%NDI_RUNTIME_DIR_V6%`). Without it, the Inputs tab says so
  and everything else works. Video only (BGRA with alpha, or opaque BGRX). NDI® is a registered
  trademark of Vizrt NDI AB.

## External control (Companion)

Status bar → **Remote** turns on an HTTP + WebSocket API on the network (port 7410, with an access
key), for Bitfocus Companion or any script. Off by default. Reference:
[`docs/external-control.md`](docs/external-control.md). Companion module:
[`../companion-module-tessera`](../companion-module-tessera/README.md).

## Keyboard

Keys work when no field or button has focus (Esc leaves a field).

| Key | Does |
|---|---|
| Space, ⌘/Ctrl+Enter | GO: Preview to Program |
| → | Load the next cue into Preview (when no layer is selected) |
| A–Z, 0–9, F1–F12 | A cue's shortcut: load it into Preview (right-click a cue → Shortcut…) |
| Arrows | Nudge the selected Preview layer 1 px (Shift: 10 px) |
| `[` / `]` | Move the selected Preview layer down / up its stack (⌘/Ctrl: to the bottom / top) |
| Esc | Deselect the layer |
| ⌘/Ctrl+Z, ⇧⌘/Ctrl+Z | Undo, redo |
| ⌘/Ctrl+S, ⇧⌘/Ctrl+S | Save, Save As |

## Known limits

- **Zero-copy decode.** Only hardware-decoded frames are drawn in place (macOS VideoToolbox,
  Windows D3D12VA on DX12). Software-decoded clips (FFV1, DNxHR; ProRes on Windows) and frames
  the import can't take (e.g. a driver that decodes into texture arrays) are copied to the GPU,
  on their decode thread. Intel's legacy Windows driver line (6th–10th gen GPUs) always gets the
  copy path: its D3D12 video driver crashes ([docs/windows-testing.md](docs/windows-testing.md)).
- **Frame pacing.** A single render loop drives all outputs, so they are all paced at the rate
  of the slowest display.
- **Audio drift.** Audio isn't slaved to the video clock, so long clips can drift.
- **Windows.** CI builds the installer, FFmpeg DLLs included. It has run on one Windows 10 PC
  with an Intel UHD 630, not yet on NVIDIA or AMD GPUs or on Windows 11. On that GPU the copy
  path keeps up with about 20 fps of 4K60 video.
