#!/usr/bin/env bash
# Build a macOS .app bundle for Ordnung.
#
# What it does:
#   1. cargo build --release -p ordnung-gui
#   2. Rasterizes tools/icon.svg → 1024×1024 PNG
#   3. Generates a full macOS iconset (16, 32, 64, 128, 256, 512, 1024)
#   4. Packs the iconset → Ordnung.icns via `iconutil`
#   5. Assembles the .app: Contents/MacOS, Contents/Resources, Info.plist
#
# Run again any time after rebuilding the binary; the .app is rewritten in place.

set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
src_icon="$here/tools/icon.svg"
bin="$here/target/release/Ordnung"
out_app="$here/Ordnung.app"
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
echo
echo "Built: $out_app"
echo "Drag it to /Applications, then to the Dock to pin."
