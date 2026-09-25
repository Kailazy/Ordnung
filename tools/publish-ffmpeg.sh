#!/usr/bin/env bash
# Publish the ffmpeg builds Ordnung downloads on first launch
# (crates/ordnung-core/src/tools.rs). Fetches the latest static macOS release
# builds from ffmpeg.martin-riedl.de for both CPU architectures, checks the
# published SHA-256 when one is served, proves the native one runs, gzips
# each binary and uploads them to the rolling `ffmpeg` GitHub release as
# ffmpeg-<version>-macos-{arm64,x86_64}.gz. Prints the version at the end:
# set FFMPEG_VERSION in tools.rs to it and ship a release.
#
# Usage: tools/publish-ffmpeg.sh          (or `make ffmpeg-publish`)
set -euo pipefail

base="https://ffmpeg.martin-riedl.de/redirect/latest/macos"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
native="$(uname -m)"   # arm64 or x86_64
version=""

for pair in arm64:arm64 amd64:x86_64; do
  up="${pair%%:*}"   # upstream's name
  tag="${pair##*:}"  # ours (matches std::env::consts::ARCH mapping)
  d="$work/$tag"
  mkdir -p "$d"
  echo "==> Fetching $tag build"
  final="$(curl -fsSL -o "$d/ffmpeg.zip" -w '%{url_effective}' "$base/$up/release/ffmpeg.zip")"
  if curl -fsSL -o "$d/ffmpeg.zip.sha256" "$final.sha256" 2>/dev/null; then
    want="$(awk '{print $1}' "$d/ffmpeg.zip.sha256")"
    have="$(shasum -a 256 "$d/ffmpeg.zip" | awk '{print $1}')"
    [[ "$want" == "$have" ]] || { echo "SHA-256 mismatch for $tag" >&2; exit 1; }
    echo "    checksum OK"
  else
    echo "    (no checksum served for $final; relying on TLS)"
  fi
  unzip -oq "$d/ffmpeg.zip" -d "$d"
  [[ -f "$d/ffmpeg" ]] || { echo "zip for $tag did not contain an ffmpeg binary" >&2; exit 1; }
  chmod 755 "$d/ffmpeg"
  if [[ "$tag" == "$native" ]]; then
    banner="$("$d/ffmpeg" -version | head -1)"
    echo "    $banner"
    version="$(echo "$banner" | awk '{print $3}' | cut -d- -f1)"
  fi
done
[[ -n "$version" ]] || { echo "could not read the version from the native binary" >&2; exit 1; }

echo "==> Packaging $version"
for tag in arm64 x86_64; do
  gzip -9 -c "$work/$tag/ffmpeg" > "$work/ffmpeg-$version-macos-$tag.gz"
  ls -la "$work/ffmpeg-$version-macos-$tag.gz"
done

echo "==> Uploading to the rolling ffmpeg release"
gh release view ffmpeg >/dev/null 2>&1 || gh release create ffmpeg --prerelease \
  --title "ffmpeg for Ordnung" \
  --notes "Unmodified static macOS builds of ffmpeg from https://ffmpeg.martin-riedl.de (FFmpeg is GPL/LGPL licensed; sources and build scripts are published there). Ordnung downloads the build for your Mac into ~/.ordnung/bin on first launch so the Convert action works without installing anything."
gh release upload ffmpeg "$work"/ffmpeg-"$version"-macos-*.gz --clobber
echo
echo "Published ffmpeg $version. Now set FFMPEG_VERSION = \"$version\" in crates/ordnung-core/src/tools.rs."
