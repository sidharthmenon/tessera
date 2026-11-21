#!/bin/sh
# Build tessera-engine and copy it to where Tauri expects the sidecar:
# app/src-tauri/binaries/tessera-render-<target triple>. The sidecar has its own name so
# tauri-build's copy (target/<profile>/tessera-render) never replaces cargo's
# target/<profile>/tessera-engine.  Usage: build-engine.sh [release]
set -eu
root="$(cd "$(dirname "$0")/.." && pwd)"
profile="${1:-debug}"
triple="$(rustc -vV | sed -n 's/^host: //p')"
case "$triple" in *windows*) exe=.exe ;; *) exe= ;; esac
if [ "$profile" = release ]; then
  cargo build --manifest-path "$root/Cargo.toml" -p tessera-engine --release
else
  cargo build --manifest-path "$root/Cargo.toml" -p tessera-engine
fi
mkdir -p "$root/app/src-tauri/binaries"
cp "${CARGO_TARGET_DIR:-$root/target}/$profile/tessera-engine$exe" "$root/app/src-tauri/binaries/tessera-render-$triple$exe"
