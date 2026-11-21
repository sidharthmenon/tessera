#!/bin/sh
# Make a built Tessera.app run without Homebrew: copy every non-system dylib the tessera-render
# sidecar loads (a recursive `otool -L` walk: FFmpeg and everything it links) into
# Contents/Frameworks, point every load path and install name at @executable_path/../Frameworks,
# raise LSMinimumSystemVersion to what those dylibs need, and ad-hoc re-sign.
# Runs on the .app that `tauri build` made; `cd app && npm run release` does both.
# Usage: bundle-ffmpeg-macos.sh [path/to/Tessera.app]
set -eu
root="$(cd "$(dirname "$0")/.." && pwd)"
app="${1:-${CARGO_TARGET_DIR:-$root/target}/release/bundle/macos/Tessera.app}"
fw="$app/Contents/Frameworks"
at="@executable_path/../Frameworks"

# Points $1's non-system load paths at Frameworks, copying in (and queueing) dylibs not there yet.
# Signatures are removed while editing (install_name_tool invalidates them) and redone at the end.
todo=""
fix() {
  for ref in $(otool -L "$1" | tail -n +2 | awk '{print $1}'); do
    case "$ref" in
      /usr/lib/*|/System/*|"$at"/*) continue ;;
      # ponytail: no @rpath/@loader_path resolution; none of Homebrew's FFmpeg deps use it today.
      @*) echo "bundle-ffmpeg-macos: $1 loads $ref; only absolute paths are handled" >&2; exit 1 ;;
    esac
    name="$(basename "$ref")"
    if [ ! -e "$fw/$name" ]; then
      cp "$ref" "$fw/$name" # follows Homebrew's opt/ symlinks to the real file
      chmod u+w "$fw/$name"
      codesign --remove-signature "$fw/$name"
      install_name_tool -id "$at/$name" "$fw/$name"
      todo="$todo $name"
    fi
    install_name_tool -change "$ref" "$at/$name" "$1"
  done
}
codesign --remove-signature "$app/Contents/MacOS/tessera-render" # fails first if there is no app
mkdir -p "$fw"
fix "$app/Contents/MacOS/tessera-render"
while [ -n "$todo" ]; do
  set -- $todo; name="$1"; shift; todo="$*" # dylib names have no spaces; the app path may
  fix "$fw/$name"
done

# Homebrew bottles target the build Mac's macOS: refuse to launch on older ones instead of
# letting the sidecar fail to load its dylibs in a respawn loop.
min="$(for l in "$fw"/*.dylib; do otool -l "$l" | awk '/minos/ {print $2}'; done | sort -t. -k1,1n -k2,2n | tail -1)"
[ -z "$min" ] || plutil -replace LSMinimumSystemVersion -string "$min" "$app/Contents/Info.plist"

codesign --force -s - "$fw"/*.dylib "$app/Contents/MacOS/tessera-render"
codesign --force -s - "$app"
codesign --verify --deep --strict "$app"
echo "bundle-ffmpeg-macos: $(ls "$fw" | wc -l | tr -d ' ') dylibs in $fw, macOS >= ${min:-?}, ad-hoc signed"
