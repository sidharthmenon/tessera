#!/bin/sh
# Build tessera-engine and copy it to where Tauri expects the sidecar:
# app/src-tauri/binaries/tessera-engine-<target triple>.  Usage: build-engine.sh [release]
set -eu
root="$(cd "$(dirname "$0")/.." && pwd)"
profile="${1:-debug}"
triple="$(rustc -vV | sed -n 's/^host: //p')"
if [ "$profile" = release ]; then
  cargo build --manifest-path "$root/Cargo.toml" -p tessera-engine --release
else
  cargo build --manifest-path "$root/Cargo.toml" -p tessera-engine
fi
mkdir -p "$root/app/src-tauri/binaries"
cp "$root/target/$profile/tessera-engine" "$root/app/src-tauri/binaries/tessera-engine-$triple"
