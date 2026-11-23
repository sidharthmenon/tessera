# Windows bring-up: handoff

The first run of Tessera on a Windows PC, 2026-11-23, on `perf/live-show` (`ad5c0c6` to `8078c39`).
For whoever picks it up next, on macOS first: what changed, which shared code to check again
there, and what is still open. The Windows runbook is [windows-testing.md](windows-testing.md).

## The PC

- Windows 10 Pro 21H2 (build 19044); Intel UHD Graphics 630, driver 31.0.101.2114 (Aug 2022);
  6-core i7, 8 GB RAM; two 1920×1080 monitors on that GPU.
- FFmpeg n9.0.2 (BtbN LGPL shared build), Rust 1.100 (MSVC), VS 2022 Build Tools 17.14, Node 24.

## Runbook results

| Check | Result |
|---|---|
| Build, `cargo clippy --workspace --all-targets` | Pass, no warnings |
| `cargo test --workspace` | Pass after the fixes below (it failed in four places at first) |
| `npm install`, `npm run check`, `npm test` | Pass |
| D3D12 debug layer | Pass: no errors drawing zero-copy NV12 and P010 frames (`d3d12_debug_layer_finds_no_errors`) |
| 4K H.264 and 4K HEVC Main10 in PVW and PGM | Correct picture. One `zero-copy off: Intel driver …` line, by design (below). Frames dropped: see Performance |
| Copy path (`TESSERA_ZERO_COPY=0`) | Correct picture; it is the default on this GPU |
| Display output on a second monitor, Identify | Pass (after `75f06c4`; before it the engine crashed in a loop) |
| Output windows presenting their halves of one 3840×1080 screen | Pass |
| Unplug and replug the output's monitor | Pass: closed when Windows dropped the monitor, reopened about 4 s after replug |
| Previews: PVW/PGM switched 10 times, Screen Management, minimise and restore | Pass: canvas, output monitor and outputs stayed live |
| `powercfg /requests` | Pass: engine under DISPLAY and SYSTEM |
| Task Manager: no power throttling for the engine or the app | Pass (after `e8d3626`) |
| Outputs stay on past the screen-off time | Not tested: that PC never turns its display off on AC power |
| Kill the engine | Pass: restarted in about 1.5 s, outputs back |
| Force-quit the UI | Pass: outputs kept playing; the next launch adopted the engine (*engine held*) |
| Restart or stop an adopted engine | Pass (after `3bbb394`; before it the engine kept running) |
| DirectShow capture | Lists and opens (virtual cameras only, so no unplug test) |
| NDI input, with and without alpha | Pass |
| Soak | Not run |
| Release build (`+crt-static`, `npx tauri build --bundles nsis`) | Pass: a 53 MiB NSIS installer |

### Performance (release build, one 4K60 H.264 layer, three outputs, previews on)

- Copy path: `perf: 196 loops, busy mean 3.70 ms max 8.31 ms, acquire max 126.01 ms, 66 dropped`.
  About 20 fps: the GPU can't keep up with decoding on D3D11, mapping and uploading each frame.
- Zero-copy forced (`TESSERA_ZERO_COPY=1`): `perf: 600 loops, busy mean 1.79 ms max 5.30 ms, acquire
  max 15.77 ms, 10 dropped`. 60 fps; the 10 drops per 10 s are 60 fps video on a 59 Hz display.

## Commits

| Commit | What | Code macOS runs too |
|---|---|---|
| `b9ba152` | App tests: absolute paths per platform, the engine binary's `.exe` | Tests only (unix paths unchanged) |
| `519aa07` | `max_non_sampler_bindings` capped at 65,536 (DX12 made a million-descriptor heap per device) | **Yes**: `Gpu::new` |
| `3cce6cf` | No UB on a DirectShow device with no media types | No |
| `5e666bc` | wgpu's warnings and errors forwarded to `log!` (they went nowhere) | **Yes**: `logger.rs`, `Gpu` |
| `c5f9d12` | Intel's legacy driver line decodes with D3D11VA (copy path) | `zero_copy::d3d12va` is false off Windows |
| `d2efe7e`, `f201b68` | Decode and render tests take turns on the GPU (`tests/common`) | **Yes**: the test suites |
| `8f22409` | Debug-layer test, runbook updates | Test is Windows-only |
| `e8d3626` | EcoQoS opt-out falls back to execution speed only on Windows 10 | No |
| `75f06c4` | Display windows go always-on-top only once their swap chain exists; a lost surface is dropped before its replacement is made | **Yes**: `outputs.rs` (`keep_on_top` is a no-op on macOS) |
| `3bbb394` | Stopping an adopted engine on Windows (TerminateProcess after checking the executable) | No |
| `6f7a28f` | README: checklist ticks, known limits | Docs |
| `8078c39` | A refused surface configuration is skipped and retried every 5 s instead of panicking the engine | **Yes**: `outputs.rs` |

## Shared code to check on macOS

1. **`Gpu::new` limits** (`519aa07`). The device asks for at most 65,536 non-sampler bindings
   (`min` with the adapter's) instead of the adapter's own limit. Check on Metal that devices still
   come up and the render, decode and compositor tests pass (`descriptor_heap_stays_small` asserts
   the cap on every backend).
2. **wgpu log forwarding** (`5e666bc`). Every wgpu record at warn or above now reaches
   `engine.log` as `gpu WARN: …` / `gpu ERROR: …`. Check a normal session for new noise from the
   Metal backend; anything frequent should be filtered in `logger::forwarded`, not ignored.
3. **Output surfaces** (`75f06c4`, `8078c39`). `configure` now runs inside a wgpu error scope and
   returns an error; the macOS drawable timeout is set only on success. An output that wasn't
   configured is skipped and gets a new surface every 5 s; a `Lost` surface is replaced with the
   old one dropped first. Check on macOS: a Display output on a second monitor (simple
   fullscreen, screen-saver level), one on the main display (normal level), Window outputs,
   Identify, unplug and replug, and sleep/wake or a resolution change (the `Outdated` and `Lost`
   paths). The log must not show `can't present`.
4. **Zero-copy expectations** (`c5f9d12`). `assert_zero_copy` now asks `zero_copy::d3d12va`, which
   is false on macOS, so the macOS condition is `cfg!(target_os = "macos")` alone, as before.
   `videotoolbox_decodes_fixtures` and the zero-copy assertions must still pass.
5. **Test suites** (`d2efe7e`, `f201b68`). GPU tests in `tests/decode.rs` and `tests/render.rs`
   run one at a time now (a lock held until the test's thread ends). Expect them to take longer;
   check nothing waits forever.

## Still open

- **Intel legacy driver crash.** On driver 31.0.101.2114, once a second D3D12VA decoder on the
  device has been freed while another lives, that one's next decode reads null inside the driver.
  It is single-threaded and repeatable; the debug layer reports nothing; D3D11VA is unaffected.
  That driver line (build below 101.3000) gets the copy path, which costs about two thirds of the
  frame rate at 4K. Worth trying the newest driver for that GPU with `TESSERA_ZERO_COPY=1`; if the
  crash is gone, narrow `zero_copy::legacy_intel_driver`.
- **Restarting an adopted engine** brings Program back empty: the controller never knew the
  adopted engine's Program, though the restart dialog says Program comes back with its clips.
  Not Windows-specific.
- **Windows menu** has no Quit item (`menu: no Quit item found; quitting from the menu won't ask`).
  Closing the window still asks.
- **Not tested on Windows:** the screen-off timeout, a soak, a 4K ProRes clip, 4×4K (one layer
  already falls short on the copy path), a USB capture device, NVIDIA and AMD GPUs, Windows 11.

## Notes for the next Windows session

- Debug builds run with the D3D12 debug layer whenever Graphics Tools is installed. Windows 10's
  layer sends its messages only to an attached debugger, so they reach neither wgpu nor
  `engine.log`; `d3d12_debug_layer_finds_no_errors` reads them from the info queue.
- D3D12 hands out one `ID3D12Device` per adapter per process: every `Gpu` in a test process
  shares it (and its info queue), whatever wgpu shows.
- `npm run tauri dev` can't rebuild while an engine started from `target/debug` still runs
  (Windows locks `tessera-render.exe`). Stop it first, or after an adopted-engine test, build
  elsewhere with `CARGO_TARGET_DIR`.
- Vite reloading the page during startup (its dependency optimisation, or an `npm run build`
  running alongside) can swallow the *Recover unsaved work?* prompt. Nothing is lost: the
  `.prev` file stays.
- Clicking or typing into Task Manager isn't possible from tools that drive the UI (it runs
  elevated); ask whoever is at the PC.
