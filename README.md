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
- Windows (untested): the BtbN FFmpeg *shared* build, with `FFMPEG_DIR` pointing at it

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

## Release build (macOS)

```sh
cd app && npm run release
```

This produces `target/release/bundle/macos/Tessera.app` (under `$CARGO_TARGET_DIR` if that is
set). The app runs without Homebrew or a dev setup. The command does two things:

1. `tauri build --bundles app` builds the release engine and the app.
2. `scripts/bundle-ffmpeg-macos.sh` copies every non-system dylib the sidecar needs into
   `Contents/Frameworks`: FFmpeg and its codec libraries, found with `otool -L`. It rewrites
   their load paths to `@executable_path/../Frameworks` and ad-hoc signs the app.

Limits of the release build:

- **macOS version and CPU.** The app runs only on the build Mac's architecture. It also needs at
  least the macOS version that the build Mac's Homebrew bottles target. The script writes that
  version into `LSMinimumSystemVersion`, so build on a Mac running the same macOS as the show Mac,
  or an older one.
- **Not notarized.** If macOS blocks the app after a download or AirDrop, run
  `xattr -dr com.apple.quarantine Tessera.app`.
- **Logs.** To see the controller and `[engine]` logs, start the app from a terminal:
  `Tessera.app/Contents/MacOS/tessera-app`.

Plain `npm run tauri build` still works, but its `.app` and `.dmg` load FFmpeg from Homebrew.

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
  `~/Library/Application Support/dev.tessera.controller/engine.json` (pid, port and token), so
  there is no blackout:
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

## Not yet verified on real hardware

- [ ] Unplugging and replugging a real monitor
- [ ] A Display output on a second monitor, checked visually
- [ ] Output windows actually presenting their halves, and Identify
- [ ] 4×4K at 60 fps in the release build with both previews watched
- [ ] A 4K ProRes 422 HQ clip at 60 fps. Make one with
      `ffmpeg -f lavfi -i testsrc2=s=3840x2160:r=60:d=10 -c:v prores_ks -profile:v 3 -pix_fmt yuv422p10le -color_range tv -colorspace bt709 -color_primaries bt709 -color_trc bt709 prores422hq.mov`
      (~650 MB, don't commit it). Import it, or put it in the spike clip folder and run
      `tessera-engine --demo-4k <folder>`, which adds it under the other four clips.
- [ ] App Nap: leave the engine with no output window visible for 10 minutes (UI in front,
      outputs covered or none). The status bar must never show *engine restarting*: the engine
      has to keep sending its 1 s stats.
- [ ] Windows: D3D11VA decode and surface sizing
- [ ] A USB capture device (Magewell/Elgato) as a live input: add, show, unplug and replug
- [ ] Windows: DirectShow capture listing and opening
- [ ] An NDI source (NDI Tools' Test Patterns) as a live input, with and without alpha
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

- **Zero-copy decode.** Decoded frames are copied to the GPU. The spike
  (`spike/engine-spike/FINDINGS.md`) found that this holds 4×4K60. Zero-copy
  (CVMetalTextureCache) becomes necessary at 3 or more concurrent 4K ProRes 4444 alpha layers,
  where 4 layers ran at 39 fps, or at more than 8 4K layers.
- **Frame pacing.** A single render loop drives all outputs, so they are all paced at the rate
  of the slowest display.
- **Audio drift.** Audio isn't slaved to the video clock, so long clips can drift.
- **Windows.** Untested, and the release build doesn't bundle FFmpeg DLLs there.
- **Engine adoption** after a UI crash works only on macOS and other unix systems.
