# Testing on Windows

A runbook for whoever brings Tessera up on a Windows PC for the first time, person or agent.
Everything so far was built and tested on macOS. The Windows code compiles and passes clippy for
`x86_64-pc-windows-msvc`, but it has never run. The job: build it, run the tests and the app,
check the Windows-only paths below, and fix what breaks.

## What only Windows exercises

- **Zero-copy decode** (`crates/engine/src/zero_copy.rs`, `win` module). Hardware decode uses
  D3D12VA on the engine's own DX12 device, and the frames are drawn in place as NV12/P010
  textures. Before a frame is used, the decode thread waits for the decoder's fence. Texture
  arrays, other formats and any import error fall back to copying the frame through staging
  buffers (`staging.rs`).
- **Intel's legacy driver line** (6th–10th gen GPUs, driver build `101.2xxx` and older, such as
  `31.0.101.2114` on UHD 630). Its D3D12 video driver crashes: once a second D3D12VA decoder on
  the device has been freed while another lives, that one's next decode reads null inside the
  driver. The D3D12 debug layer reports nothing before the crash, and D3D11VA is unaffected. So
  on that line the engine decodes with D3D11VA and copies, and logs one
  `zero-copy off: Intel driver …` line saying so (`zero_copy::d3d12va`). `TESSERA_ZERO_COPY=1`
  forces zero-copy anyway, for checking a newer driver of that line.
- **Backends.** DX12 is the default (`gpu.rs`) unless `WGPU_BACKEND` is set.
  `TESSERA_ZERO_COPY=0` switches to D3D11VA plus copying.
- **Power** (`crates/engine/src/os.rs`, and `stay_responsive` in `app/src-tauri/src/lib.rs`).
  `SetThreadExecutionState` keeps the display and the system awake. The engine and the
  controller both opt out of EcoQoS.
- **Thread priorities.** The render loop runs at `ABOVE_NORMAL`, decode at `NORMAL`, and preview
  JPEG and probe threads at `BELOW_NORMAL`.
- **Platform code.** Output windows and fullscreen, surface sizing, DirectShow capture.

## Setup

1. Install:
   - Git for Windows (the scripts are `sh`; run them from Git Bash or PowerShell with `sh` on PATH).
   - Rust stable with the MSVC toolchain.
   - Visual Studio 2022 Build Tools with *Desktop development with C++*.
   - LLVM (`winget install LLVM.LLVM`), which FFmpeg's bindings need.
   - Node 20+.
   - WebView2, which is already on Windows 11.
2. Install FFmpeg 9, the same build CI uses: download
   `ffmpeg-n9.0-latest-win64-lgpl-shared-9.0.zip` from the BtbN/FFmpeg-Builds releases and unzip
   it. Then, in each new PowerShell:

   ```powershell
   $env:FFMPEG_DIR = "C:\ffmpeg\ffmpeg-n9.0-latest-win64-lgpl-shared-9.0"   # where you unzipped it
   $env:LIBCLANG_PATH = "C:\Program Files\LLVM\bin"
   $env:PATH = "$env:FFMPEG_DIR\bin;$env:PATH"   # its DLLs, for the tests and the engine
   ```

3. Clone the repository and check out the branch to test. Run every command below from its
   `tessera/` folder.

## Build and automated tests

```powershell
sh scripts/build-engine.sh
cargo clippy --workspace --all-targets
cargo test --workspace
cd app; npm install; npm run check; npm test; cd ..
```

All of it should pass. The engine tests need a GPU. On DX12, the decode tests also check that
hardware-decoded clips were imported without a copy (`assert_zero_copy`,
`d3d12_frames_are_imported`). On Intel's legacy driver line they expect the copy path instead,
and `d3d12_frames_are_imported` is skipped. The decode tests that use the GPU run one at a time
(`gpu()` in `tests/decode.rs`).

If a decode test fails, run it again with `$env:TESSERA_ZERO_COPY = "0"`. If it passes that way,
the import is at fault, not the decode.

Then check with the D3D12 debug layer. Add the *Graphics Tools* optional feature in Windows
Settings and restart. Debug builds then always run with the layer (wgpu turns validation on in
debug builds; `$env:WGPU_VALIDATION = "1"` does it for a release build). Run the tests again:

```powershell
cargo test -p tessera-engine
```

`d3d12_debug_layer_finds_no_errors` draws zero-copy NV12 and P010 frames and fails on any error
the layer recorded, resource states and fences included. It reads them from the device's info
queue: Windows 10's layer sends its messages to the debug output, where wgpu would pass them to
`engine.log`, only while a debugger is attached. Without the layer the test says it was skipped.
On Intel's legacy driver line it draws copied frames; to check zero-copy there, run it with
`$env:TESSERA_ZERO_COPY = "1"` (it may crash, as described above).

## Manual checks in the app

Start the app with `cd app; npm run tauri dev`. It finds its engine log at
`%LOCALAPPDATA%\dev.tessera.controller\logs\engine.log`. Every 10 s the log gets a `perf:` line
with the mean and max render-loop time per frame (vsync wait excluded) and the dropped frames.

1. **Hardware clips.** Play a 4K H.264 clip and a 4K HEVC Main10 clip in PVW and PGM.
   - The log must have no `zero-copy off` line, except the single `zero-copy off: Intel driver …`
     line on Intel's legacy driver line.
   - Expect 0 dropped frames. On macOS a release build spends about 0.3 ms per frame.
   - BtbN's LGPL build has no x264/x265, so use real footage, encode with a hardware encoder if
     present (`h264_nvenc`, `hevc_nvenc`, `h264_amf`, `h264_qsv`), or make the clips elsewhere.
2. **Copy path.** Set `$env:TESSERA_ZERO_COPY = "0"`, then start `npm run tauri dev` again. The
   same clips must look the same.
3. **Outputs.**
   - A Display output on a second monitor goes fullscreen and shows its part of the layout.
   - Identify draws the white border.
   - Unplug and replug that monitor.
4. **Previews.**
   - Switch the canvas between Program and Preview about ten times.
   - Open and close Screen Management.
   - Minimise the app and restore it.
   - The canvas and the output monitor must stay live, never black.
5. **Power.** While the app runs:
   - `powercfg /requests` (admin) lists the engine (`tessera-render.exe`) under DISPLAY and
     SYSTEM.
   - With the app in the background, Task Manager → Details shows neither `tessera-render.exe`
     nor `tessera-app.exe` in Efficiency mode.
   - Leave the PC untouched past its screen-off time. The outputs must stay on.
6. **Supervision.** Items 10 and 11 of the README's *Before the first show* checklist:
   - Kill the engine: the controller restarts it.
   - Force-quit the UI: the outputs keep playing, and the next launch adopts the engine.
7. **Capture.** If a capture device is at hand, check that DirectShow lists it, opens it, and
   survives an unplug and replug.
8. **Soak (if time allows).** Run a looping show with a few layers for hours. Check that:
   - memory stays flat (Task Manager);
   - the status bar never shows *engine restarting*;
   - a looping clip's audio stays in sync.

Check the performance numbers in a release build, built the way CI builds it. First copy
`$env:FFMPEG_DIR\bin\*.dll` into `app/src-tauri/ffmpeg-dlls/`, then:

```powershell
sh scripts/build-engine.sh release
cd app; npx tauri build --bundles nsis --config src-tauri/tauri.release-windows.conf.json
```

## Reporting back

- For each check: pass or fail, plus the GPU model and driver, and the Windows version.
- The relevant `engine.log` lines: `perf:` lines, any `zero-copy off`, any GPU or device-lost
  errors.
- Fix bugs on the branch, with a test where one fits. In the README's *Not yet verified on real
  hardware* list, tick the items that now pass.
- This repository is public: keep machine-specific paths and other products' names out of code,
  docs and commit messages.
