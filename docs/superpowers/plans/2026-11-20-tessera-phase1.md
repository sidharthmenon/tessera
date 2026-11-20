# Tessera Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A macOS-first, Windows-capable Rust media server: separate render engine (multi-output, multi-screen, multi-layer, hw video, transitions) driven by a Tauri + Svelte controller with cue grid and screen management.

**Architecture:** Cargo workspace. `crates/show` = pure model + ops + cue logic + engine protocol (serde, ts-rs). `crates/engine` = headless binary (winit + wgpu + ffmpeg-next + cpal + axum) that renders PVW/PGM and serves WS control + MJPEG previews. `app/` = Tauri 2 controller owning the `Show`, spawning the engine as sidecar; Svelte 5 frontend.

**Tech Stack:** Rust 1.100, wgpu 30, winit 0.30, ffmpeg-next 9.x (Homebrew FFmpeg 9), cpal, axum 0.8 + tokio, tokio-tungstenite, image 0.25, serde/serde_json, uuid, ts-rs 12, Tauri 2 + tauri-plugin-dialog, Svelte 5 + Vite + TypeScript.

**Spec:** `docs/superpowers/specs/2026-11-20-tessera-design.md`

## Global Constraints

- macOS (Apple silicon) + Windows; first show on macOS. Windows verified before its first use.
- Engine binds `127.0.0.1` only; every WS / MJPEG request must carry `?token=<TESSERA_TOKEN>`; wrong/missing token → HTTP 401.
- Engine never panics on bad input (missing file, bad codec, unknown id, malformed JSON): log + `FromEngine::Error`, keep rendering.
- Show file: JSON, extension `.tshow`, `version: 1`; atomic write (tmp + rename); autosave `<file>.autosave` debounced 2 s; undo cap 100.
- Take = all screens at once; cue defines screen iff `layers_by_screen` contains its key (empty vec = explicit black).
- Shared player: incoming layer reuses PGM player when `(screen_id, media_id, layer index)` match.
- New PVW players paused on first frame; start on Take.
- Performance target: 4 simultaneous 4K layers (HEVC/H.264/ProRes 4444) at 60 fps on show Mac.
- Own name, logo and artwork. Dark UI.
- Spike findings (`spike/engine-spike/FINDINGS.md`, if present) override crate versions and the zero-copy decision in Tasks 7–8.

## Review Focus

1. **Media file missing/moved/unsupported after reopening a show** — layer renders transparent, `Error{message}` names the path, other layers keep playing, engine stays up. Test: Task 7 `open_missing_file_is_err`, Task 9 `failed_player_does_not_block_take`.
2. **Configured monitor unplugged / renamed** — output becomes Unbound, other outputs keep rendering, rebinds when it returns. Test: Task 6 `bind_monitor_*`.
3. **Engine crashes mid-show** — controller restarts it and resends config + PGM + PVW; outputs come back. Test: Task 11 `resync_messages_order`.
4. **Corrupt / future-version / half-written show file** — open fails with clear error, file untouched; save never leaves a truncated file. Test: Task 11 `open_garbage_is_err`, `open_future_version_is_err`, `save_is_atomic`.
5. **Operator double-presses Take mid-transition** — running transition snaps to end, new one starts, no leaked players. Test: Task 9 `take_during_transition_snaps`.

---

## File Structure

```
Cargo.toml                         workspace members: crates/show, crates/engine, app/src-tauri
crates/show/src/lib.rs             re-exports
crates/show/src/geom.rs            Rect + mapping math
crates/show/src/model.rs           Show, Output, Screen, Mapping, MediaItem, Group, Cue, Layer…
crates/show/src/ops.rs             ShowOp + Show::apply (cascading removals)
crates/show/src/cue.rs             grid ops, take_merge, changed_screens, resolve_shared
crates/show/src/protocol.rs        ToEngine, FromEngine, EngineConfig, MonitorInfo…
crates/show/tests/export_ts.rs     writes app/src/lib/bindings/*.ts
crates/engine/src/main.rs          args/env, winit event loop, wiring
crates/engine/src/gpu.rs           device/queue, pipelines, texture helpers
crates/engine/src/compositor.rs    scene → screen textures → output/preview textures
crates/engine/src/shaders/*.wgsl   layer.wgsl, transition.wgsl, map.wgsl
crates/engine/src/outputs.rs       window per Output, monitor binding
crates/engine/src/player.rs        video/image player, decode thread, frame queue
crates/engine/src/audio.rs         cpal mixer
crates/engine/src/buses.rs         PVW/PGM state, player pool, Take/transition
crates/engine/src/net.rs           axum WS + MJPEG, token check
crates/engine/src/probe.rs         probe + thumbnail
crates/engine/tests/fixtures/      gen.sh + small clips (see Task 7)
app/src-tauri/src/main.rs          Tauri builder, commands
app/src-tauri/src/controller.rs    Controller state, undo, live state (pvw/pgm)
app/src-tauri/src/engine_client.rs sidecar spawn + WS client + resync
app/src-tauri/src/files.rs         open/save/autosave
app/src/App.svelte, app/src/lib/*.svelte, app/src/lib/api.ts, app/src/lib/bindings/*.ts
```

---

### Task 1: Workspace, geometry, model

**Files:**
- Create: `Cargo.toml`, `crates/show/Cargo.toml`, `crates/show/src/{lib,geom,model}.rs`
- Test: inline `#[cfg(test)]` in `geom.rs`, `model.rs`

**Interfaces:**
- Produces (all `#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]`, serde `rename_all = "camelCase"`, enums `#[serde(tag = "type")]`; ids are `uuid::Uuid`):
  - `Rect { x: f32, y: f32, w: f32, h: f32 }`; `Rect::intersect(&self, &Rect) -> Option<Rect>`; `Rect::map_between(&self, from: &Rect, to: &Rect) -> Rect` (affine map of self from `from`-space into `to`-space); `Rect::uv_in(&self, outer: &Rect) -> Rect` (normalized 0..1).
  - `Show { version: u32, outputs: Vec<Output>, screens: Vec<Screen>, mappings: Vec<Mapping>, media: Vec<MediaItem>, groups: Vec<Group>, master_volume: f32 }`; `Show::new() -> Show` (version 1, one group "Group 1", master_volume 1.0).
  - `Output { id, name: String, node: String, target: OutputTarget, width: u32, height: u32, brightness: f32 }`
  - `OutputTarget::Display { monitor_name: String, monitor_x: i32, monitor_y: i32 } | Window { x: i32, y: i32 }`
  - `Screen { id, name: String, width: u32, height: u32, layout_x: i32, layout_y: i32 }`
  - `Mapping { id, screen_id: Uuid, src: Rect, output_id: Uuid, dst: Rect }`
  - `MediaItem { id, name: String, path: String, kind: MediaKind, width: u32, height: u32, duration_ms: Option<u64>, has_audio: bool, has_alpha: bool, thumb: Option<String> }`; `MediaKind::Image | Video`
  - `Group { id, name: String, cues: Vec<Cue> }`
  - `Cue { id, name: String, slot: u32, layers_by_screen: BTreeMap<Uuid, Vec<Layer>>, transition: Transition }`
  - `Transition { kind: TransitionKind, duration_ms: u32 }`; `TransitionKind::Cut | Fade | WipeLeft | WipeRight`
  - `Layer { id, name: String, source: Source, rect: Rect, opacity: f32, brightness: f32, contrast: f32, saturation: f32, volume: f32, playback: Playback, visible: bool }`; `Layer::new(source: Source, rect: Rect) -> Layer` (fresh id, opacity 1, brightness 0, contrast 1, saturation 1, volume 1, Loop, visible).
  - `Source::Media { media_id: Uuid } | Pattern { pattern: Pattern }`; `Pattern::Bars | Grid | Gradient | Solid { rgba: [f32; 4] }`; `Playback::Loop | Once | HoldLast`
  - `pub type Scene = BTreeMap<Uuid, Vec<Layer>>;`

- [ ] **Step 1:** Workspace `Cargo.toml` (`resolver = "3"`, members `crates/show` only for now; engine/app added in their tasks). Package names: `tessera-show`, `tessera-engine`, `tessera-app` (rename the Tauri scaffold's package in Task 11). `show` deps: `serde` (derive), `serde_json`, `uuid` (`v4`, `serde`), `ts-rs` (features `uuid-impl`, `serde-compat`).
- [ ] **Step 2: Write failing tests**
  - `rect_intersect_disjoint_is_none`: `Rect{0,0,10,10}` ∩ `Rect{20,20,5,5}` → `None`.
  - `rect_intersect_overlap`: `{0,0,10,10}` ∩ `{5,5,10,10}` → `Some({5,5,5,5})`.
  - `rect_map_between_half_split`: screen 3840×1080 left half `{0,0,1920,1080}` mapped from `from={0,0,1920,1080}` to `to={0,0,1920,1080}` is identity; `{960,0,960,1080}.map_between(&{0,0,1920,1080}, &{100,0,960,540})` → `{580,0,480,540}`.
  - `rect_uv_in`: `{960,0,960,1080}.uv_in(&{0,0,3840,1080})` → `{0.25,0,0.25,1}`.
  - `show_json_roundtrip`: `Show::new()` with one output, screen, mapping, cue with one pattern layer → `serde_json` round-trip equals original; JSON contains `"type":"Display"` and `"layersByScreen"`.
- [ ] **Step 3:** Run `cargo test -p tessera-show` → FAIL (not defined).
- [ ] **Step 4:** Implement types and methods above.
- [ ] **Step 5:** Run `cargo test -p tessera-show` → PASS.
- [ ] **Step 6:** `git add -A && git commit -m "feat(show): workspace, geometry, show model"`

### Task 2: Show ops with cascading removals

**Files:** Create `crates/show/src/ops.rs`; test inline.

**Interfaces:**
- Consumes: Task 1 model.
- Produces: `ShowOp` (serde tag `"op"`): `AddOutput{output}`, `UpdateOutput{output}`, `RemoveOutput{id}`, `AddScreen{screen}`, `UpdateScreen{screen}`, `RemoveScreen{id}`, `SetMapping{mapping}` (upsert by id), `RemoveMapping{id}`, `AddMedia{items: Vec<MediaItem>}`, `UpdateMedia{item}`, `RemoveMedia{id}`, `AddGroup{name}`, `RenameGroup{id, name}`, `RemoveGroup{id}`, `SetCue{group_id, cue}`, `MoveCue{group_id, from: u32, to: u32}`, `DuplicateCue{group_id, from: u32, to: u32}`, `DeleteCue{group_id, slot: u32}`, `SetMasterVolume{volume}`.
- `Show::apply(&mut self, op: ShowOp) -> Result<(), String>` — unknown id → `Err`, show unchanged.

- [ ] **Step 1: Failing tests**
  - `remove_screen_cascades`: removing a screen deletes its mappings and removes its key from every cue's `layers_by_screen`.
  - `remove_output_cascades`: deletes mappings with that `output_id`.
  - `remove_media_cascades`: removes every layer whose source is that media, in all cues.
  - `remove_last_group_is_err`: show must keep ≥1 group.
  - `unknown_id_is_err_and_unchanged`: `UpdateScreen` with unknown id → `Err`, `show == before`.
  - `master_volume_clamped`: `SetMasterVolume{volume: 3.0}` → `1.0`; `-1.0` → `0.0`.
- [ ] **Step 2:** Run → FAIL. **Step 3:** Implement; cue grid variants call Task 3's `set_cue`/`move_cue`/`duplicate_cue`/`delete_cue` (same crate — implement Tasks 2 and 3 together, commit separately). **Step 4:** Run → PASS.
- [ ] **Step 5:** Commit `feat(show): ShowOp with cascading removals`.

### Task 3: Cue logic

**Files:** Create `crates/show/src/cue.rs`; test inline.

**Interfaces:**
- Produces:
  - `set_cue(group: &mut Group, cue: Cue)` — replaces cue with same slot.
  - `move_cue(group: &mut Group, from: u32, to: u32) -> Result<(), String>` — swaps if `to` occupied; `Err` if `from` empty.
  - `duplicate_cue(group: &mut Group, from: u32, to: u32) -> Result<(), String>` — fresh cue id and fresh layer ids; `Err` if `from` empty or `to` occupied.
  - `delete_cue(group: &mut Group, slot: u32)`
  - `next_cue_slot(group: &Group, after: Option<u32>) -> Option<u32>` — lowest occupied slot `> after` (or lowest overall when `None`); no wrap.
  - `take_merge(pgm: &Scene, incoming: &Scene) -> Scene`
  - `changed_screens(old: &Scene, new: &Scene) -> Vec<Uuid>`
  - `resolve_shared(pgm: &Scene, incoming: &Scene) -> HashMap<Uuid, Uuid>` — incoming layer id → pgm layer id, matched on same screen, same `Source::Media{media_id}`, same index. Patterns never shared.

- [ ] **Step 1: Failing tests**
  - `move_cue_swaps`, `duplicate_cue_fresh_ids` (no layer id equal between original and copy), `duplicate_to_occupied_is_err`.
  - `next_cue_slot_skips_gaps`: slots {1,3,7}: `None→1`, `Some(1)→3`, `Some(7)→None`.
  - `take_merge_keeps_undefined_screens`: pgm {A:[a], B:[b]}, incoming {A:[c]} → {A:[c], B:[b]}.
  - `take_merge_empty_vec_blacks_screen`: incoming {B:[]} → B present and empty.
  - `changed_screens_detects_only_diffs`.
  - `resolve_shared_same_media_same_index`: pgm A:[bg(m1)], incoming A:[bg'(m1), ov(m2)] → `{bg'.id → bg.id}`; different index or screen → not shared.
- [ ] **Step 2:** Run → FAIL. **Step 3:** Implement. **Step 4:** Run → PASS.
- [ ] **Step 5:** Commit `feat(show): cue grid ops, take merge, shared player resolution`.

### Task 4: Engine protocol + TS bindings

**Files:** Create `crates/show/src/protocol.rs`, `crates/show/tests/export_ts.rs`; output `app/src/lib/bindings/`.

**Interfaces:**
- Produces:
  - `EngineConfig { outputs: Vec<Output>, screens: Vec<Screen>, mappings: Vec<Mapping>, media: Vec<MediaItem> }` (media paths absolute — controller resolves).
  - `Bus::Pvw | Pgm`; `TransportAction::Play | Pause | Stop | Restart`
  - `ToEngine` (tag `"type"`): `SetConfig{config}`, `SetPreview{scene}`, `SetProgram{scene}`, `Take{transition}`, `Transport{bus, layer: Option<Uuid>, action}`, `SetMasterVolume{volume}`, `ListMonitors`, `Probe{media_id, path}`, `Identify{output_id}`.
  - `MonitorInfo { name, x: i32, y: i32, width: u32, height: u32, scale: f64, refresh_mhz: Option<u32> }`
  - `OutputState { output_id, bound: bool, monitor: Option<String> }`
  - `ProbeInfo { kind: MediaKind, width, height, duration_ms: Option<u64>, has_audio, has_alpha, thumb: Option<String> }`
  - `FromEngine` (tag `"type"`): `Monitors{monitors}`, `OutputStatus{outputs: Vec<OutputState>}`, `Probed{media_id, info: Option<ProbeInfo>, error: Option<String>}`, `TakeDone`, `Positions{positions: BTreeMap<Uuid, u64>}`, `Stats{fps: f32, dropped: u64}`, `Error{message}`.
- [ ] **Step 1: Failing tests** `to_engine_json_shape`: `ToEngine::ListMonitors` → `{"type":"listMonitors"}`; `Take` round-trips. `export_ts` test calls `ShowOp::export_all_to("../../app/src/lib/bindings")` etc. for `Show`, `ShowOp`, `ToEngine`, `FromEngine` and asserts `Show.ts` exists.
- [ ] **Step 2:** FAIL → **Step 3:** implement → **Step 4:** `cargo test -p tessera-show` PASS, bindings present.
- [ ] **Step 5:** Commit `feat(show): engine protocol and TS bindings`.

### Task 5: Engine compositor (headless, patterns + images + mapping + effects)

**Files:** Create `crates/engine/Cargo.toml`, `src/{gpu,compositor}.rs`, `src/shaders/{layer,map}.wgsl`, `tests/render.rs`; add `crates/engine` to workspace.

**Interfaces:**
- Consumes: `Scene`, `EngineConfig`, `Rect` math.
- Produces:
  - `Gpu::new_headless() -> anyhow::Result<Gpu>`; `Gpu::new_for_surface(instance, &surface)`.
  - `trait FrameSource { fn texture(&self) -> Option<LayerTexture>; }` — `LayerTexture` enum: `Rgba(TextureView) | Nv12{y, uv} | P010{y, uv}` plus `premultiplied: bool`, `full_range: bool`. Images/patterns implement it; video player in Task 7.
  - `Compositor::new(&Gpu) -> Compositor`
  - `Compositor::render_screen(&mut self, gpu, screen: &Screen, layers: &[(Layer, Option<LayerTexture>)]) -> &TextureView` (RGBA8 premultiplied, screen-sized, cached per screen id).
  - `Compositor::render_output(&mut self, gpu, target: &TextureView, output: &Output, mappings: &[Mapping], screens: &HashMap<Uuid, TextureView>)` — each mapping draws `src.uv_in(screen rect)` into `dst`, × `output.brightness`.
  - `Compositor::render_layout(&mut self, gpu, screens: &[Screen], views, width: u32) -> TextureView` (preview: screens at `layout_x/y`, scaled).
  - `read_rgba(gpu, &Texture) -> Vec<u8>` test/preview helper.
- Shaders: patterns generated in `layer.wgsl` (Bars = 8 SMPTE-ish vertical bars, Grid = 64 px white lines on black + center cross, Gradient = horizontal black→white, Solid). Layer uniform: rect (in screen px), opacity, brightness (add), contrast (mul around 0.5), saturation (mix with luma); YUV→RGB BT.709 limited/full.
- [ ] **Step 1: Failing tests** (`tests/render.rs`, headless Metal):
  - `solid_layer_fills_rect`: 100×100 screen, Solid red layer at `{0,0,50,100}` → pixel (25,50) red, (75,50) transparent black.
  - `opacity_half`: Solid white opacity 0.5 → pixel ≈ (128,128,128,128) ±2.
  - `screen_split_across_two_outputs`: screen 200×100 left half red / right half blue; output A maps `src{0,0,100,100}`→`dst{0,0,100,100}`, output B maps `src{100,0,100,100}`→same; A center red, B center blue.
  - `mapping_outside_bounds_clips`: mapping `dst` partly outside output → no panic, inside pixels drawn.
  - `brightness_zero_output_black`.
- [ ] **Step 2:** `cargo test -p tessera-engine --test render` FAIL → **Step 3:** implement → **Step 4:** PASS.
- [ ] **Step 5:** Commit `feat(engine): headless compositor with patterns, effects, mapping`.

### Task 6: Output windows, monitor binding, engine main loop

**Files:** Create `src/outputs.rs`, `src/main.rs`.

**Interfaces:**
- Consumes: Compositor, `EngineConfig`, `MonitorInfo`, `OutputState`.
- Produces:
  - `bind_monitor(target: &OutputTarget, monitors: &[MonitorInfo]) -> Option<usize>` — Display: exact name match first, then exact `(x,y)` position, else `None`; Window → `None`.
  - `Outputs::apply_config(&mut self, event_loop: &ActiveEventLoop, gpu, config: &EngineConfig, monitors: &[MonitorInfo]) -> Vec<OutputState>` — create/reposition/close windows; Display bound → borderless fullscreen on monitor (macOS `with_simple_fullscreen`), unbound → no window; Window → decorated window at x,y with `width×height` inner size.
  - Engine event loop: `UserEvent::Command(ToEngine)` via `EventLoopProxy`; `about_to_wait` renders all outputs; primary output `PresentMode::Fifo`, others `Mailbox` if supported else `Immediate`/`Fifo`. macOS: `ActivationPolicy::Accessory`. Re-run binding on monitor change (poll `available_monitors()` every 2 s; emit `OutputStatus` on change).
  - `Identify{output_id}` → overlay big output name on that output for 3 s (text via a tiny baked bitmap font or just flash white border — builder's choice).
- [ ] **Step 1: Failing tests** (inline, pure): `bind_monitor_by_name`, `bind_monitor_falls_back_to_position`, `bind_monitor_missing_is_none`.
- [ ] **Step 2:** FAIL → **Step 3:** implement → **Step 4:** PASS.
- [ ] **Step 5: Manual check:** `cargo run -p tessera-engine -- --demo` (demo flag loads a built-in config: 2 Window outputs, 1 screen 3840×1080 with Bars split across both) → two windows show left/right halves of the bars; unplug/replug second monitor with a Display output → Unbound/Bound logged.
- [ ] **Step 6:** Commit `feat(engine): output windows and monitor binding`.

### Task 7: Video player with hardware decode

**Files:** Create `src/player.rs`, `tests/decode.rs`, `tests/fixtures/gen.sh` (+ generated tiny clips committed: `h264_64.mp4` 1 s 64×64 30 fps with AAC tone, `prores4444_64.mov` 1 s 64×64 with alpha = 0 in left half).

**Interfaces:**
- Consumes: `LayerTexture`, `Playback`, `MediaItem`.
- Produces:
  - `Player::open(path: &Path, playback: Playback, clock: Clock) -> anyhow::Result<Player>` — spawns decode thread; hwaccel VideoToolbox (macOS) / D3D11VA (Windows) when codec supported, else software; `av_hwframe_transfer_data` → planes; frame queue capacity 4.
  - `Player::play(&self, at: Instant)`, `pause()`, `stop()` (seek 0, paused, show first frame), `restart(at)`; starts paused on first frame.
  - `Player::current_frame(&mut self, now: Instant) -> Option<LayerTexture>` — uploads newest frame with `pts <= position(now)` (reuses textures); Loop seeks 0 at EOF with pre-roll; Once → transparent after end; HoldLast → last frame.
  - `Player::position_ms(&self, now: Instant) -> u64`; `Player::audio(&self) -> Option<AudioTap>` (Task 8).
  - `ImageSource::open(path) -> anyhow::Result<ImageSource>` (via `image`, premultiply on load).
  - Decode helper used by tests: `decode_all_frames(path) -> anyhow::Result<Vec<DecodedFrameInfo { pts_ms: u64, format: PixelFormatKind }>>`.
- [ ] **Step 1:** `gen.sh` creates fixtures with `ffmpeg` CLI; commit fixtures (< 200 KB total).
- [ ] **Step 2: Failing tests:** `decode_h264_frame_count` (30 ±1 frames, pts strictly increasing), `prores4444_has_alpha` (decoded format is alpha-capable, left-half alpha sample < 0.1, right-half > 0.9 after upload+readback via Compositor), `open_missing_file_is_err`, `loop_wraps_position` (position at 1.5 s of a 1 s Loop clip ≈ 500 ms), `once_ends_transparent`.
- [ ] **Step 3:** FAIL → **Step 4:** implement (if spike recommends zero-copy, add `LayerTexture::External` Metal path here behind `cfg(target_os="macos")`) → **Step 5:** PASS.
- [ ] **Step 6: Perf check:** `cargo run --release -p tessera-engine -- --demo-4k <dir>` plays 4 × 4K clips; `Stats.fps ≥ 59`, `dropped` not rising.
- [ ] **Step 7:** Commit `feat(engine): hw-accelerated video player`.

### Task 8: Audio mixer

**Files:** Create `src/audio.rs`.

**Interfaces:**
- Produces: `AudioTap` (per-player ring buffer of f32 interleaved at device rate/channels, fed by decode thread via `swresample`); `Mixer::start() -> anyhow::Result<Mixer>` (cpal default output; no device → `Err`, engine continues silent); `Mixer::set_taps(&self, taps: Vec<(AudioTap, f32 /*volume*/)>)`; `Mixer::set_master(&self, v: f32)`.
- [ ] **Step 1: Failing test** `mix_sums_and_clamps`: pure `mix_into(out: &mut [f32], taps: &[(&[f32], f32)], master: f32)` — two taps at 0.8 vol 1.0 → clamped 1.0; volume 0.5 halves.
- [ ] **Step 2:** FAIL → **Step 3:** implement → **Step 4:** PASS. **Step 5: Manual:** fixture tone audible in `--demo`.
- [ ] **Step 6:** Commit `feat(engine): cpal audio mixer`.

### Task 9: Buses, player pool, Take + transitions

**Files:** Create `src/buses.rs`, `src/shaders/transition.wgsl`.

**Interfaces:**
- Consumes: `resolve_shared`, `take_merge`, `changed_screens`, `Player`, `Compositor`.
- Produces:
  - `PlayerPool` keyed by player id with `Arc` refcount; `trait Opener` (real = `Player::open`, test = fake) so tests run without FFmpeg.
  - `Buses::set_preview(&mut self, scene: Scene, now)` — resolves shared layers against PGM, opens new players paused.
  - `Buses::set_program(&mut self, scene: Scene)` — no transition, players kept by layer id.
  - `Buses::take(&mut self, t: Transition, now: Instant)` — PGM := `take_merge(pgm, pvw)`; plays new PVW players at `now`; outgoing kept until done; Cut = instant; `changed_screens` only get transition.
  - `Buses::tick(&mut self, now) -> Option<FromEngine>` (returns `TakeDone` when transition ends, frees unreferenced players).
  - `Buses::frame_layers(&mut self, bus: Bus, screen_id, now) -> Vec<(Layer, Option<LayerTexture>)>`; transition progress `Buses::transition(screen_id, now) -> Option<(TransitionKind, f32)>`.
  - Transition shader: mix(outgoing, incoming, p) for Fade; WipeLeft/Right hard edge at p·width.
- [ ] **Step 1: Failing tests** (fake opener):
  - `shared_player_identity_across_take` (same `Arc::ptr_eq` before/after).
  - `new_pvw_players_paused_until_take`.
  - `undefined_screen_untouched_by_take`; `explicit_empty_screen_goes_black`.
  - `players_freed_after_transition`.
  - `take_during_transition_snaps`: Take, then Take again at 50% → first outgoing freed immediately, only one transition active.
  - `failed_player_does_not_block_take`: opener returns `Err` for one layer → that layer renders `None`, Take still completes, `Error` emitted.
- [ ] **Step 2:** FAIL → **Step 3:** implement → **Step 4:** PASS.
- [ ] **Step 5:** Commit `feat(engine): PVW/PGM buses, shared players, transitions`.

### Task 10: Engine network, probe, thumbnails

**Files:** Create `src/net.rs`, `src/probe.rs`.

**Interfaces:**
- Produces:
  - CLI: `tessera-engine --port <u16> [--demo]`; token from env `TESSERA_TOKEN` (required unless `--demo`).
  - `GET /ws?token=` — JSON `ToEngine` in, `FromEngine` out (broadcast to all clients); malformed JSON → `Error` reply, connection kept.
  - `GET /preview/pgm.mjpg?token=`, `/preview/pvw.mjpg?token=` — `multipart/x-mixed-replace; boundary=frame`, JPEG ~960 px wide, ≤15 fps, quality builder's choice; readback only while ≥1 subscriber.
  - `GET /thumb/{media_id}.jpg?token=` — serves cached thumbnail.
  - `probe(path: &Path, thumb_dir: &Path) -> anyhow::Result<ProbeInfo>` — image or video metadata; 160 px JPEG thumb (video: frame at 10% duration).
  - `Positions` at 5 Hz, `Stats` at 1 Hz.
- [ ] **Step 1: Failing tests** (tokio, in-process server on port 0): `ws_rejects_bad_token` (401), `ws_list_monitors_replies` (any `Monitors` reply — allow empty list in CI), `ws_malformed_json_returns_error`, `probe_fixture_video` (64×64, has_audio, duration ≈ 1000 ±50), `probe_missing_is_err`.
- [ ] **Step 2:** FAIL → **Step 3:** implement → **Step 4:** PASS.
- [ ] **Step 5:** Commit `feat(engine): WS control, MJPEG previews, probe`.

### Task 11: Tauri controller

**Files:** Scaffold `app/` with `npm create tauri-app@latest app -- --template svelte-ts --manager npm` then edit; create `app/src-tauri/src/{controller,engine_client,files}.rs`; add `app/src-tauri` to workspace; `tauri.conf.json` `bundle.externalBin: ["binaries/tessera-engine"]`; add `tauri-plugin-dialog`.

**Interfaces:**
- Consumes: `Show`, `ShowOp`, `Scene`, `ToEngine`, `FromEngine`, `EngineConfig`, cue functions.
- Produces:
  - `files::open(path: &Path) -> Result<Show, String>` (rejects `version > 1` with message "show file is from a newer Tessera"); `files::save_atomic(path: &Path, show: &Show) -> Result<(), String>`; autosave debounced 2 s to `<path>.autosave`.
  - `Controller { show, path: Option<PathBuf>, undo: Vec<Show>, redo: Vec<Show>, pvw: Option<CueRef>, pgm: Scene, pvw_scene: Scene }`, `CueRef { group_id: Uuid, slot: u32 }`.
  - `Controller::apply(&mut self, op: ShowOp) -> Result<Vec<ToEngine>, String>` — push undo (cap 100), clear redo, return engine messages (SetConfig if outputs/screens/mappings/media changed; SetPreview if PVW cue edited; SetProgram if a cue on PGM edited).
  - `Controller::undo() / redo() -> Vec<ToEngine>`; `load_cue(CueRef) -> Vec<ToEngine>`; `take() -> Vec<ToEngine>` (uses cue's transition; PVW cue stays loaded); `load_next() -> Vec<ToEngine>` (via `next_cue_slot`).
  - `Controller::resync_messages(&self) -> Vec<ToEngine>` — `[SetConfig, SetProgram{pgm}, SetPreview{pvw_scene}, SetMasterVolume]` in that order.
  - `engine_client`: spawn sidecar (`tauri_plugin_shell` sidecar) with random 32-byte hex token in `TESSERA_TOKEN`, port picked by controller (free port); connect WS; on disconnect: respawn if process exited, reconnect with backoff (250 ms → 2 s), then send `resync_messages`. Forward `FromEngine` to frontend as `engine-event`.
  - Tauri commands: `get_show() -> Show`, `apply_op(op: ShowOp)`, `undo()`, `redo()`, `load_cue(group_id, slot)`, `take()`, `load_next()`, `transport(bus, layer, action)`, `import_media(paths: Vec<String>)` (adds items then sends `Probe` per item; `Probed` → `UpdateMedia`), `open_show(path)`, `save_show(path: Option<String>)`, `engine_info() -> {port, token}` (for MJPEG URLs). Emits `show-changed` (full `Show`) after every mutation.
- [ ] **Step 1: Failing tests** (inline, no Tauri runtime): `undo_redo_roundtrip`, `undo_cap_100`, `apply_screen_change_emits_set_config`, `edit_pvw_cue_emits_set_preview`, `take_emits_take_with_cue_transition`, `resync_messages_order`, `open_garbage_is_err`, `open_future_version_is_err`, `save_is_atomic` (save over existing file; simulate failure by saving into a read-only dir → `Err`, original intact).
- [ ] **Step 2:** FAIL → **Step 3:** implement → **Step 4:** `cargo test -p tessera-app` PASS.
- [ ] **Step 5:** Build script copies `target/{debug,release}/tessera-engine` to `app/src-tauri/binaries/tessera-engine-<target-triple>` (Tauri sidecar naming).
- [ ] **Step 6:** Commit `feat(app): Tauri controller with engine sidecar, undo, file IO`.

### Task 12: Frontend main window

**Files:** `app/src/App.svelte`, `app/src/lib/{api.ts,store.svelte.ts}`, `app/src/lib/panels/{Resources,Canvas,OutputMonitor,Properties,CueGrid,Transport}.svelte`, `app/src/app.css`.

**Interfaces:**
- Consumes: bindings `Show`, `ShowOp`, `FromEngine`; Tauri commands from Task 11.
- Produces: `api.ts` typed wrappers per command; `store` with `$state` show + engine events + selection (`selectedLayer`, `activeGroup`, `canvasTab: 'pgm'|'pvw'`).
- Layout (dark theme, orange accent): menu/toolbar top; Resources left (tabs Local / Patterns; list with thumb, resolution, duration; drag to canvas = add layer to PVW cue at screen under cursor, full-screen rect); Canvas center with PGM/PVW tabs showing `<img src="http://127.0.0.1:{port}/preview/{bus}.mjpg?token=…">` + absolutely-positioned layer boxes (drag/resize → `SetCue`); Output Monitor right-top (PGM mjpg); Properties right (selected layer: rect, opacity, brightness, contrast, saturation, volume, playback; cue transition kind + duration); transport bar (play/pause/stop/restart for selected layer bus, master volume, GO button); bottom cue grid (group tabs + add group; 27+ slots; click = `load_cue`, double-click = load + take, right-click menu: rename / duplicate / delete / clear screen; empty slot click = create cue from current PVW). Status bar: engine connected, fps, dropped.
- Hotkeys: `Space` take, `ArrowRight` load next, `Cmd/Ctrl+Z` undo, `Shift+Cmd/Ctrl+Z` redo, `Cmd/Ctrl+S` save. Ignore hotkeys while focus in an input.
- [ ] **Step 1:** Configure CSP in `tauri.conf.json` to allow `img-src 'self' http://127.0.0.1:*`.
- [ ] **Step 2:** Implement panels.
- [ ] **Step 3: Verify:** `cd app && npm run check` (svelte-check, no errors) and `npm run tauri dev` → add a pattern layer to PVW, see it in PVW preview, Space → appears in PGM and output window.
- [ ] **Step 4:** Commit `feat(ui): main window panels, cue grid, hotkeys`.

### Task 13: Screen Management dialog

**Files:** `app/src/lib/screens/{ScreenManager,AddOutput,MappingEditor,LayoutEditor}.svelte`.

**Interfaces:**
- Consumes: `Monitors`, `OutputStatus` events; `ShowOp` output/screen/mapping ops; `Identify`.
- Behavior: modal with left column Outputs tree (node "Local" → outputs, eye = Identify, trash = remove) and Screens list (add/remove, reorder); Add Output dialog listing `Monitors` (name, position, resolution, Bound/Unbound) plus "Window" entry, name + W/H fields; Mapping editor (top canvas: selected output with mapped screen slices, drag/resize dst; numeric src/dst X/Y/W/H); Layout editor (bottom canvas: screens at layout positions, drag; numeric X/Y/W/H). Apply / OK / Cancel — edits buffered locally, Apply sends ops in sequence (single undo step not required).
- [ ] **Step 1:** Implement. **Step 2: Verify:** `npm run check`; in `tauri dev` create 2 Window outputs + screen 3840×1080 mapped half/half → windows show halves of Bars; Identify flashes the right window.
- [ ] **Step 3:** Commit `feat(ui): screen management dialog`.

### Task 14: Packaging + README

**Files:** `README.md`, `app/src-tauri/tauri.conf.json`, `scripts/bundle-ffmpeg-macos.sh`.

- [ ] **Step 1:** `scripts/bundle-ffmpeg-macos.sh` copies FFmpeg dylibs used by `tessera-engine` (`otool -L`) into the app's `Contents/Frameworks` and rewrites paths with `install_name_tool` to `@executable_path/../Frameworks`.
- [ ] **Step 2:** README: prerequisites (Rust stable, Node 20+, `brew install ffmpeg pkgconf`; Windows: BtbN FFmpeg shared build + `FFMPEG_DIR`), dev (`cargo build -p tessera-engine && cd app && npm run tauri dev`), build, show-day checklist from the spec's Verification section.
- [ ] **Step 3: Verify:** `cd app && npm run tauri build` → `.app` launches on a clean user account without Homebrew in PATH (`env -i`), engine starts (status bar connected).
- [ ] **Step 4:** Commit `chore: packaging and README`.

---

## Final verification

- `cargo test --workspace` all green.
- Spec Verification checklist (design doc) run end-to-end in `npm run tauri dev`, including kill-engine and force-quit-UI checks and the 4×4K 60 fps check.
