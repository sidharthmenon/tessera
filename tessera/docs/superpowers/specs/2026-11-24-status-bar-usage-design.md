# Status bar: CPU and RAM use

A small change. The status bar shows how much CPU and RAM Tessera uses and how much headroom the
machine has left, so an operator can see during a show whether Tessera is getting heavy or the
machine is running out of room because of anything else on it.

## Goals

- Tessera's use: CPU and RAM of the engine and the app (controller) together.
- The machine's use: CPU, and RAM used out of the total.
- Updated about once a second, on macOS and Windows.

## Non-goals

- GPU use, graphs or history, per-thread numbers, alerts or toasts.
- Showing the numbers over the remote (Companion) API.

## Anti-goals

- Measuring that loads or stalls the controller: no scan of every process on the machine, nothing
  on the controller lock or the engine connection.
- A status bar that no longer fits: the numbers stay short.

## Approaches

1. **The app measures (picked).** A thread in the controller samples the engine's process (its pid
   is known, also for an adopted engine), its own process and the machine once a second, and emits
   the numbers to the UI. Counts both Tessera processes and keeps the engine's process free of
   extra work. Costs one timer and one event.
2. **The engine measures.** Adds its own and the machine's numbers to the `Stats` it already sends
   every second. No new timer or event, but it can't see the app's process (on Windows the app's
   WebView can use hundreds of MB), so "Tessera" would be the engine only.

Picked: 1, for a Tessera figure that counts the app too.

## Design

- **Measuring:** the `sysinfo` crate (system feature only) in the controller: global CPU, total and
  available memory, and only the two processes refreshed (engine pid and the app's own pid).
- **CPU:** both figures are a share of the whole machine (a process's per-core figure divided by
  the logical core count), so they compare directly: Tessera 9% with the machine at 23% means
  other programs use 14%.
- **RAM:** Tessera is the resident memory of the two processes. The machine's used RAM is what the OS
  reports as used (macOS: Activity Monitor's Memory Used; Windows: total minus available).
- **Status bar:** `Tessera CPU 9% · 0.6 GB    Machine CPU 23% · RAM 9.8 / 18 GB`, after the fps and
  dropped figures. Hovering the Tessera figure splits it: engine and app.
- **Warning colour** (the status bar's existing `.bad` style): machine CPU at 85% or more, or less
  than 10% of RAM available.
- **Engine down or restarting:** the Tessera figure counts what is running (the app alone).

## Known limits

- macOS runs the UI's web pages in WebKit helper processes that belong to the system, not the
  app; their share is not counted. It is small next to the engine's.
- On Windows the app's WebView2 runs in separate msedgewebview2.exe processes, which are not counted
  either.

## Left to the builder

- The event's name and type, how it reaches the store, and where the formatting lives.
- One unit test for the share-of-machine and headroom arithmetic, one for the UI formatting.
