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
- **Autosave.** Tessera autosaves 2 s after the last edit:
  - titled show: to `<show>.tshow.autosave` next to it;
  - untitled show: to `~/Library/Application Support/dev.tessera.controller/untitled.tshow.autosave`.
- **Recovery.** Unsaved work from an earlier session is moved to `….autosave.prev`. The app then
  asks *Recover unsaved work?*. This happens at startup for the untitled autosave, and when you
  open a show whose autosave is newer than the show file. *Recover* opens the work as an
  untitled show; save it to keep it. *Not now* leaves the `.prev` file in place, and you can open
  it later with Open. An older `.prev` is never overwritten: later ones are kept as
  `….autosave.<time>.prev`, and the newest is the one offered.
- **Saving** removes the show's autosave, and quitting writes any edit the 2 s autosave hasn't
  caught yet.
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
- **UI force-quit.** The engine survives a force-quit or crash of the UI, and its outputs keep
  playing. On relaunch the controller adopts the engine through
  `~/Library/Application Support/dev.tessera.controller/engine.json` (pid, port and token), so
  there is no blackout:
  - The status bar shows *engine held*, and the outputs stay untouched until you open or create
    a show.
  - After that, edits to the on-air cue reach air on the next GO.
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
3. Import an image and an mp4.
4. Build two cues:
   - cue 1: looping background video + image;
   - cue 2: the same background video + a ProRes 4444 alpha overlay.
5. Load cue 2 to PVW. PGM doesn't change, and the overlay is paused on its first frame.
6. Press Space to fade. The background loop doesn't restart, the overlay starts on Take, and its
   alpha is correct.
7. Take a cue that touches only screen A. Screen B is untouched.
   - With the taken cue still in PVW, move a layer on the Preview tab. Program doesn't change: the
     edit goes to a copy in the first free slot (status bar: *Copied to slot N*). Live edits
     happen only on the Program tab.
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
- [ ] Menu bar and Dock behaviour while an output is in simple fullscreen

## Displays and audio

- **Main display.** A Display output on the main display (the one with the menu bar) stays at
  normal window level, so ⌘-Tab or the Dock icon brings the controller back in front. The menu bar
  may show over that output. On any other display an output sits above every window, the
  controller's included. Add Output marks the monitor the controller is on as *Controller
  display* and asks first.
- **Audio output.** The engine picks the macOS default output device when it starts. Set the
  default output (System Settings → Sound) before launching Tessera. After unplugging or
  replugging the audio interface, restart the engine: `pkill -f tessera-render`. The controller
  starts a new one, and its clips restart from their first frame (see Engine supervision).

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
