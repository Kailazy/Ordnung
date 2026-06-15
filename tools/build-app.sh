#!/usr/bin/env bash
# Build, sign, and install the macOS .app bundle for Ordnung.
#
# Usage:
#   tools/build-app.sh            # build → sign → install to /Applications → relaunch
#   tools/build-app.sh --no-install   # build the local Ordnung.app only, don't touch /Applications
#   tools/build-app.sh --no-launch    # install but don't relaunch the app
#
# What it does:
#   1. cargo build --release -p ordnung-gui
#   2. Rasterizes tools/icon.svg → 1024×1024 PNG, regenerates the iconset → .icns
#   3. Assembles the .app (Contents/MacOS, Resources, Info.plist)
#   4. Deep ad-hoc codesigns with a stable identity (icon + permissions persist)
#   5. Installs to /Applications, registers with LaunchServices, relaunches
#
# Run it any time after editing the GUI; one command refreshes the Dock app.

set -euo pipefail

install=1
launch=1
for arg in "$@"; do
  case "$arg" in
    --no-install) install=0 ;;
    --no-launch)  launch=0 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

here="$(cd "$(dirname "$0")/.." && pwd)"
src_icon="$here/tools/icon.svg"
bin="$here/target/release/Ordnung"
out_app="$here/Ordnung.app"
installed_app="/Applications/Ordnung.app"
work="$here/tools/.app-build"

echo "==> Rendering icons"
rm -rf "$work"
mkdir -p "$work/icon.iconset"
# Render the source SVG at 1024 first, then downscale with `sips` for crispness.
rsvg-convert -w 1024 -h 1024 "$src_icon" -o "$work/icon-1024.png"
# Refresh the runtime icon the GUI binary embeds via include_bytes!, so the
# in-app icon (title bar / app switcher) matches the bundle icon.
rsvg-convert -w 512 -h 512 "$src_icon" -o "$here/crates/ordnung-gui/assets/icon.png"

echo "==> Building release binary"
cargo build --release -p ordnung-gui

if [[ ! -f "$bin" ]]; then
  echo "build did not produce $bin" >&2
  exit 1
fi

# macOS iconset naming convention. `@2x` is the retina variant.
declare -a sizes=(
  "16  icon_16x16.png"
  "32  icon_16x16@2x.png"
  "32  icon_32x32.png"
  "64  icon_32x32@2x.png"
  "128 icon_128x128.png"
  "256 icon_128x128@2x.png"
  "256 icon_256x256.png"
  "512 icon_256x256@2x.png"
  "512 icon_512x512.png"
  "1024 icon_512x512@2x.png"
)
for entry in "${sizes[@]}"; do
  read -r px name <<<"$entry"
  sips -z "$px" "$px" "$work/icon-1024.png" --out "$work/icon.iconset/$name" >/dev/null
done

iconutil -c icns "$work/icon.iconset" -o "$work/Ordnung.icns"

echo "==> Assembling .app bundle"
rm -rf "$out_app"
mkdir -p "$out_app/Contents/MacOS" "$out_app/Contents/Resources"
cp "$bin" "$out_app/Contents/MacOS/Ordnung"
chmod +x "$out_app/Contents/MacOS/Ordnung"
cp "$work/Ordnung.icns" "$out_app/Contents/Resources/Ordnung.icns"

cat >"$out_app/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>                <string>Ordnung</string>
    <key>CFBundleDisplayName</key>         <string>Ordnung</string>
    <key>CFBundleIdentifier</key>          <string>app.ordnung.gui</string>
    <key>CFBundleVersion</key>             <string>0.0.1</string>
    <key>CFBundleShortVersionString</key>  <string>0.0.1</string>
    <key>CFBundleExecutable</key>          <string>Ordnung</string>
    <key>CFBundleIconFile</key>            <string>Ordnung</string>
    <key>CFBundlePackageType</key>         <string>APPL</string>
    <key>LSMinimumSystemVersion</key>      <string>11.0</string>
    <key>NSHighResolutionCapable</key>     <true/>
    <key>LSApplicationCategoryType</key>   <string>public.app-category.music</string>
</dict>
</plist>
PLIST

rm -rf "$work"

# Deep ad-hoc codesign with a STABLE identifier. Without this the bundle carries
# only the linker's per-build ad-hoc signature, whose identifier changes every
# rebuild — macOS then treats each build as a different app and re-prompts for
# file-access / media-key permissions and can drop the Dock icon to a generic
# tile. Pinning `--identifier app.ordnung.gui` keeps one stable identity so
# permissions and the custom icon persist across rebuilds.
echo "==> Code signing (ad-hoc, stable identity)"
codesign --force --deep --sign - \
  --identifier app.ordnung.gui \
  "$out_app"
codesign --verify --deep --strict "$out_app" && echo "    signature OK"

# Nudge LaunchServices/Finder to re-read the bundle icon instead of serving a
# stale cached tile.
touch "$out_app"

echo
echo "Built: $out_app"

if [[ "$install" -eq 0 ]]; then
  echo "Skipped install (--no-install). Drag it to /Applications to pin."
  exit 0
fi

# Quit a running instance so we can overwrite the bundle and relaunch fresh code.
if pgrep -x Ordnung >/dev/null 2>&1; then
  echo "==> Quitting running Ordnung"
  osascript -e 'tell application "Ordnung" to quit' >/dev/null 2>&1 || killall Ordnung 2>/dev/null || true
  # give it a moment to release the bundle
  for _ in 1 2 3 4 5; do pgrep -x Ordnung >/dev/null 2>&1 || break; sleep 0.3; done
fi

echo "==> Installing to $installed_app"
rm -rf "$installed_app"
cp -R "$out_app" "$installed_app"

# Re-register so Finder/Dock pick up the current icon + identity immediately.
lsregister="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
[[ -x "$lsregister" ]] && "$lsregister" -f "$installed_app" >/dev/null 2>&1 || true
touch "$installed_app"

# Pin to the Dock once (idempotent).
if ! defaults read com.apple.dock persistent-apps 2>/dev/null | grep -q "Ordnung.app"; then
  echo "==> Pinning to Dock"
  defaults write com.apple.dock persistent-apps -array-add "<dict><key>tile-data</key><dict><key>file-data</key><dict><key>_CFURLString</key><string>$installed_app</string><key>_CFURLStringType</key><integer>0</integer></dict></dict></dict>"
  killall Dock 2>/dev/null || true
fi

if [[ "$launch" -eq 1 ]]; then
  echo "==> Launching"
  open "$installed_app"
fi

echo
echo "Done: $installed_app (installed, signed, pinned)."
