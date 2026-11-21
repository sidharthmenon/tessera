## Downloads

| File | For |
|---|---|
| `Tessera_{{VERSION}}_macos_arm64.dmg` | macOS 12 or newer, Apple Silicon |
| `Tessera_{{VERSION}}_x64-setup.exe` | Windows 10/11, x64. **Untested so far:** run the [pre-show checklist](https://github.com/sidharthmenon/tessera/blob/main/tessera/README.md) before a show. |
| `tessera-companion-{{VERSION}}.tgz` | Bitfocus Companion 4.0 or newer: the Tessera connection module |

## Install

**macOS.** Open the DMG and drag Tessera to Applications. The app isn't notarized, so the first
start is blocked: open System Settings → Privacy & Security, scroll down and click
**Open Anyway**. Or, in Terminal: `xattr -dr com.apple.quarantine /Applications/Tessera.app`.

**Windows.** Run the setup. The installer isn't signed: on the SmartScreen warning click
**More info** → **Run anyway**.

**Companion.** In Companion: **Modules** → **Import module package** → the `.tgz`. Then add a
**Tessera** connection, and turn on remote control in Tessera (status bar → **Remote**). See
[external control](https://github.com/sidharthmenon/tessera/blob/main/tessera/docs/external-control.md).

## Third-party: FFmpeg

Tessera uses [FFmpeg](https://ffmpeg.org) under the LGPL 2.1 or later, dynamically linked (the
libraries sit next to the app and can be replaced).

- macOS: FFmpeg built from `ffmpeg-9.0.2.tar.xz` (attached to this release) with
  [`build-ffmpeg-macos.sh`](https://github.com/sidharthmenon/tessera/blob/main/tessera/scripts/build-ffmpeg-macos.sh).
- Windows: the LGPL shared build from [BtbN/FFmpeg-Builds](https://github.com/BtbN/FFmpeg-Builds),
  {{FFMPEG_WINDOWS}}. Its source and build scripts are linked there.

Tessera itself is GPL-3.0; the Companion module is MIT.
