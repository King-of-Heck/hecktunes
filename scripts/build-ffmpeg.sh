#!/usr/bin/env bash
# Builds the small ffmpeg HeckTunes uses to convert FLAC to AAC, and puts it in src-tauri/binaries/
# named the way Tauri's externalBin expects (ffmpeg-<target-triple>[.exe]).
#
# The build is LGPL (no --enable-gpl, no external libraries) and holds only what conversion needs:
# the file protocol, the FLAC demuxer/parser/decoder, the resampler, the native AAC encoder and the
# mp4/ipod muxers. Its source is the pinned release tarball below, checked against its SHA-256.
#
# Usage: scripts/build-ffmpeg.sh [<target-triple>]   (default: the host's rustc triple)
#   Windows: run in an MSYS2 CLANG64 shell (needs make and clang): the binary is linked statically.
#   macOS:   needs the Xcode command-line tools; x86_64-apple-darwin is cross-built on Apple silicon.
set -euo pipefail

VERSION=8.1.3
SHA256=7138d28c96d9d3e3af4ee3d8cad72741f8ffb40da90c1112235dea3ecd3178a3
URL="https://ffmpeg.org/releases/ffmpeg-$VERSION.tar.xz"

root="$(cd "$(dirname "$0")/.." && pwd)"
triple="${1:-$(rustc -vV | sed -n 's/^host: //p')}"
work="${FFMPEG_BUILD_DIR:-$root/src-tauri/target/ffmpeg-build}/$triple"
out_dir="$root/src-tauri/binaries"
case "$triple" in
  *windows*) exe=.exe ;;
  *) exe= ;;
esac
out="$out_dir/ffmpeg-$triple$exe"

sha256() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

mkdir -p "$work" "$out_dir"
tarball="$work/ffmpeg-$VERSION.tar.xz"
if [ ! -f "$tarball" ] || [ "$(sha256 "$tarball")" != "$SHA256" ]; then
  rm -f "$tarball"
  curl -fsSL --retry 3 -o "$tarball.part" "$URL"
  got="$(sha256 "$tarball.part")"
  if [ "$got" != "$SHA256" ]; then
    rm -f "$tarball.part"
    echo "ffmpeg: checksum mismatch for $URL: expected $SHA256, got $got" >&2
    exit 1
  fi
  mv "$tarball.part" "$tarball"
fi

src="$work/ffmpeg-$VERSION"
rm -rf "$src"
tar -xJf "$tarball" -C "$work"
cd "$src"

flags=(
  --disable-everything --disable-autodetect --disable-doc --disable-debug --disable-network
  --disable-shared --enable-static --disable-x86asm
  --disable-ffplay --disable-ffprobe --disable-avdevice --disable-swscale
  --enable-ffmpeg --enable-swresample --enable-avfilter
  --enable-protocol=file,pipe
  --enable-demuxer=flac --enable-parser=flac --enable-decoder=flac
  --enable-encoder=aac --enable-muxer=ipod,mp4
  --enable-filter=aresample,aformat,anull,atrim
)
case "$triple" in
  *windows*)
    flags+=(--cc=clang --cxx=clang++ --extra-ldflags=-static --pkg-config-flags=--static)
    ;;
  aarch64-apple-darwin)
    flags+=(--arch=arm64 --extra-cflags=-mmacosx-version-min=11.0 --extra-ldflags=-mmacosx-version-min=11.0)
    ;;
  x86_64-apple-darwin)
    flags+=(--enable-cross-compile --arch=x86_64 --target-os=darwin
      "--extra-cflags=-arch x86_64 -mmacosx-version-min=10.15" "--extra-ldflags=-arch x86_64 -mmacosx-version-min=10.15")
    ;;
esac

./configure "${flags[@]}"
jobs="$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)"
make -j"$jobs" ffmpeg$exe
if command -v strip >/dev/null; then strip "ffmpeg$exe" 2>/dev/null || true; fi
cp "ffmpeg$exe" "$out"
chmod 755 "$out"
echo "ffmpeg $VERSION (LGPL, AAC-only build) saved to $out"
