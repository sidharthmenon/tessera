# Status Bar CPU and RAM Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The status bar shows Tessera's CPU and RAM use (engine + app) and the machine's CPU and
RAM, updated once a second.

**Architecture:** A thread in the Tauri controller samples the engine's process, the app's own
process and the machine with `sysinfo` once a second and emits a `usage` event. The Svelte store
keeps the latest sample; a pure formatter in `logic.ts` turns it into the status bar's text.

**Tech Stack:** Rust (Tauri 2 controller, `sysinfo` 0.40), Svelte 5 + TypeScript (plain Node tests).

**Spec:** `tessera/docs/superpowers/specs/2026-11-24-status-bar-usage-design.md`

## Global Constraints

- All paths below are relative to `tessera/`.
- `sysinfo` with `default-features = false, features = ["system"]`; added to `app/src-tauri/Cargo.toml` only (not the engine).
- Only the two Tessera processes are refreshed each second, never every process on the machine.
- CPU figures are a share of the whole machine (0–100).
- Warning colour when machine CPU ≥ 85% or available RAM < 10% of total.
- Public repo: no machine-specific paths, usernames or other products' names in code, comments or commit messages.
- Do not commit unless the user asks.

## Review Focus

- Engine stopped, restarting or not yet spawned: Tessera's figure is the app alone; the tooltip says the engine isn't running (Task 2 test `engine down`).
- The engine pid no longer exists (exited between samples): treated as engine not running, no panic (Task 1 test `own_process_is_sampled_and_gone_pid_is_none`).
- A per-core figure above 100% on a multi-core machine, or a stray negative: the share is clamped to 0–100 (Task 1 test `share_of_machine_is_clamped`).
- Used memory reported above total: clamped to total (Task 1 test `own_process_is_sampled_and_gone_pid_is_none`).
- First second after start: `sysinfo` CPU needs two refreshes, so nothing is emitted until one interval has passed (Task 1, Step 3).

---

### Task 1: Controller samples usage and emits `usage`

**Files:**
- Create: `app/src-tauri/src/usage.rs`
- Modify: `app/src-tauri/Cargo.toml` (add `sysinfo`)
- Modify: `app/src-tauri/src/engine_client.rs` (add `Engine::pid`, next to `Engine::info` ~line 258)
- Modify: `app/src-tauri/src/lib.rs` (`pub mod usage;`, start it in `.setup` after `stay_responsive()` ~line 714, add `usage` to the event list in the module docs at the top)

**Interfaces:**
- Produces (serialized camelCase, emitted as event `usage` once a second):
  ```rust
  #[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
  #[serde(rename_all = "camelCase")]
  pub struct ProcessUsage { pub cpu: f32, pub ram: u64 }   // cpu: % of machine; ram: resident bytes
  #[derive(Clone, Debug, PartialEq, serde::Serialize)]
  #[serde(rename_all = "camelCase")]
  pub struct Usage { pub engine: Option<ProcessUsage>, pub app: ProcessUsage, pub machine_cpu: f32, pub ram_used: u64, pub ram_total: u64 }
  ```
- `pub fn spawn(app: tauri::AppHandle)` in `usage.rs`.
- `pub fn pid(&self) -> Option<u32>` on `Engine` (the pid in `proc`, adopted or child).

- [ ] **Step 1: Write the failing tests** in `usage.rs` (`#[cfg(test)] mod tests`)

```rust
#[test]
fn share_of_machine_is_clamped() {
    assert_eq!(share_of_machine(200.0, 8), 25.0);   // 2 cores busy of 8
    assert_eq!(share_of_machine(900.0, 8), 100.0);
    assert_eq!(share_of_machine(-1.0, 8), 0.0);
    assert_eq!(share_of_machine(50.0, 0), 50.0);    // core count unknown: as is
}

#[test]
fn own_process_is_sampled_and_gone_pid_is_none() {
    let mut s = Sampler::new();
    let u = s.sample(Some(1_073_741_820)); // no such process (positive as an i32 pid, a multiple of 4 for Windows)
    assert_eq!(u.engine, None, "gone pid");
    assert!(u.app.ram > 0, "own process has resident memory");
    assert!(u.ram_total > 0 && u.ram_used <= u.ram_total);
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cd app/src-tauri && cargo test --lib usage::`
Expected: compile error (`share_of_machine`, `Sampler` not defined).

- [ ] **Step 3: Implement in `usage.rs`**

- `fn share_of_machine(per_core: f32, cores: usize) -> f32`: `per_core / cores` clamped to `0.0..=100.0`; `cores == 0` → `per_core` clamped.
- Machine RAM used is `sys.used_memory()` clamped to total (not total minus available: on macOS that leaves out active memory).
- `struct Sampler { sys: sysinfo::System, own: sysinfo::Pid, cores: usize }` with `fn new() -> Sampler` (CPU list and a first CPU refresh done here, so the first `sample` after one interval has real figures) and `fn sample(&mut self, engine: Option<u32>) -> Usage`: refresh global CPU, memory, and only `own` + the engine pid (CPU and memory refresh kinds); engine `None` when the pid is absent or `sysinfo` doesn't find it.
- `pub fn spawn(app: AppHandle)`: a named std thread (`usage`): `Sampler::new()`, then loop { sleep 1 s; `sample(app.state::<Engine>().pid())`; `app.emit("usage", usage)` }. Errors from `emit` are ignored.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd app/src-tauri && cargo test --lib usage::`
Expected: 2 passed.

- [ ] **Step 5: Whole crate stays clean**

Run: `cargo clippy --workspace --all-targets && cargo test -p tessera-app`
Expected: no warnings, all tests pass.

### Task 2: Store, formatter and status bar

**Files:**
- Modify: `app/src/lib/api.ts` (types + `Events['usage']`)
- Modify: `app/src/lib/store.svelte.ts` (`usage` state ~line 34, listener next to `remote-changed` ~line 177)
- Modify: `app/src/lib/logic.ts` (add `formatUsage`)
- Modify: `app/src/lib/logic.test.ts` (tests, before the final `console.log`)
- Modify: `app/src/lib/panels/StatusBar.svelte` (render after the fps/dropped spans)

**Interfaces:**
- Consumes: event `usage` with payload matching Task 1's `Usage` (camelCase).
- Produces:
  ```ts
  // api.ts
  export type ProcessUsage = { cpu: number; ram: number };
  export type Usage = { engine: ProcessUsage | null; app: ProcessUsage; machineCpu: number; ramUsed: number; ramTotal: number };
  // logic.ts
  export function formatUsage(u: Usage): { tessera: string; machine: string; detail: string; tight: boolean };
  ```
  `ui.usage: api.Usage | null` in the store (`$state.raw`, `null` until the first event).

- [ ] **Step 1: Write the failing tests** in `logic.test.ts`

```ts
{
  const GB = 1024 ** 3;
  const u = { engine: { cpu: 8.4, ram: 0.5 * GB }, app: { cpu: 1.2, ram: 0.1 * GB }, machineCpu: 23.4, ramUsed: 9.8 * GB, ramTotal: 18 * GB };
  eq(L.formatUsage(u), { tessera: 'Tessera CPU 10% · 0.6 GB', machine: 'Machine CPU 23% · RAM 9.8 / 18 GB', detail: 'engine 8% · 0.5 GB, app 1% · 0.1 GB', tight: false }, 'usage');
  eq(L.formatUsage({ ...u, engine: null }).tessera, 'Tessera CPU 1% · 0.1 GB', 'engine down: app alone');
  eq(L.formatUsage({ ...u, engine: null }).detail, 'engine not running, app 1% · 0.1 GB', 'engine down');
  eq(L.formatUsage({ ...u, machineCpu: 85 }).tight, true, 'cpu at the limit');
  eq(L.formatUsage({ ...u, ramUsed: 16.3 * GB }).tight, true, 'under 10% RAM available');
  eq(L.formatUsage({ ...u, ramUsed: 16.1 * GB }).tight, false, 'just over 10% RAM available');
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cd app && npm test`
Expected: FAIL (`L.formatUsage is not a function`).

- [ ] **Step 3: Implement `formatUsage` in `logic.ts`**

CPU: `Math.round` to whole percent; Tessera CPU is engine + app, rounded after summing. GB: bytes / 1024³ with one decimal, a trailing `.0` dropped (`18 GB`, `0.6 GB`). `tight`: `machineCpu >= 85 || ramTotal - ramUsed < 0.1 * ramTotal`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd app && npm test`
Expected: `logic: all checks passed`.

- [ ] **Step 5: Wire the store and status bar**

`api.ts`: types above, `'usage': Usage` in `Events`. Store: `usage = $state.raw<api.Usage | null>(null)` and `api.on('usage', (u) => (ui.usage = u))` in the startup listener list. `StatusBar.svelte`, after the fps/dropped block:
```svelte
{#if ui.usage}
  {@const u = formatUsage(ui.usage)}
  <span title={u.detail}>{u.tessera}</span>
  <span class:bad={u.tight}>{u.machine}</span>
{/if}
```

- [ ] **Step 6: Check types and run the app**

Run: `cd app && npm run check && npm test`
Expected: 0 errors, `logic: all checks passed`. Then `npm run tauri dev`: the status bar shows both figures, updating each second; hovering the Tessera figure shows the engine/app split; Engine → Stop engine shows "engine not running" in the tooltip and Tessera's figure drops to the app's.
