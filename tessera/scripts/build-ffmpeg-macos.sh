#!/bin/sh
# Build the FFmpeg that release builds bundle: LGPL (no --enable-gpl, so no x264/x265), shared
# libraries, for macOS $MACOSX_DEPLOYMENT_TARGET (default 12.0) and newer. FFmpeg's defaults keep
# every decoder and demuxer plus VideoToolbox, AudioToolbox and AVFoundation; the command-line
# programs and docs are left out, and so are libraries configure would pick up from Homebrew.
# The source tarball is kept in <prefix> too: it is the corresponding source each release ships.
# Usage: build-ffmpeg-macos.sh <prefix>, then build with PKG_CONFIG_PATH=<prefix>/lib/pkgconfig.
set -eu
version=9.0.2
sha256=8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e
prefix="$1"
: "${MACOSX_DEPLOYMENT_TARGET:=12.0}"
export MACOSX_DEPLOYMENT_TARGET
src="ffmpeg-$version.tar.xz"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

curl -fsSL "https://ffmpeg.org/releases/$src" -o "$work/$src"
echo "$sha256  $work/$src" | shasum -a 256 -c -
tar -xf "$work/$src" -C "$work"
cd "$work/ffmpeg-$version"
min="-mmacosx-version-min=$MACOSX_DEPLOYMENT_TARGET"
./configure --prefix="$prefix" --enable-shared --disable-static --disable-programs --disable-doc \
  --disable-lzma --disable-libxcb --disable-sdl2 --disable-xlib \
  --extra-cflags="$min" --extra-ldflags="$min"
grep -q '^#define CONFIG_GPL 0' config.h # LGPL, or stop
make -j"$(sysctl -n hw.ncpu)"
make install
cp "$work/$src" "$prefix/$src"

# Every library may load only the system and each other, or the app would need Homebrew (and
# Homebrew's newer macOS) after all.
bad="$(for l in "$prefix"/lib/*.dylib; do otool -L "$l" | tail -n +2 | awk '{print $1}'; done | grep -v -e '^/usr/lib/' -e '^/System/' -e "^$prefix/lib/" | sort -u)"
[ -z "$bad" ] || { echo "build-ffmpeg-macos: libraries outside the system: $bad" >&2; exit 1; }
echo "build-ffmpeg-macos: LGPL FFmpeg $version in $prefix, macOS >= $MACOSX_DEPLOYMENT_TARGET"
