#!/usr/bin/env bash
# Keep Cargo's target/ from eating the disk.
#
# Cargo never prunes stale incremental caches or superseded dependency
# artifacts, so a long-lived dev tree grows without bound (this repo reached
# 195 GB, with the disk at 100%). Deleting target/debug is always safe: it is
# build output that the next `cargo build` regenerates. target/release is kept
# so `make app` stays fast. None of this exists on a user's machine; they only
# get the 28 MB app bundle.
#
#   make prune           delete target/debug if it is over PRUNE_LIMIT_GB (default 20)
#   make prune-install   install a launchd job that runs the prune daily at 04:00
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
LIMIT_GB="${PRUNE_LIMIT_GB:-20}"
LABEL=app.ordnung.prune-target
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
LOG="$HOME/.ordnung/prune.log"

if [[ "${1:-}" == "--install" ]]; then
  mkdir -p "$HOME/Library/LaunchAgents" "$HOME/.ordnung"
  cat > "$PLIST" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key>
  <array>
    <string>/bin/bash</string>
    <string>$REPO/tools/prune-target.sh</string>
  </array>
  <key>StartCalendarInterval</key>
  <dict><key>Hour</key><integer>4</integer><key>Minute</key><integer>0</integer></dict>
  <key>StandardOutPath</key><string>$LOG</string>
  <key>StandardErrorPath</key><string>$LOG</string>
</dict>
</plist>
PLIST
  launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
  launchctl bootstrap "gui/$(id -u)" "$PLIST"
  echo "prune: installed $PLIST (daily 04:00, log $LOG)"
  exit 0
fi

DEBUG="$REPO/target/debug"
if [[ ! -d "$DEBUG" ]]; then
  echo "$(date '+%F %T') prune: no target/debug, nothing to do"
  exit 0
fi
used_gb=$(( $(du -sk "$DEBUG" | cut -f1) / 1048576 ))
if (( used_gb < LIMIT_GB )); then
  echo "$(date '+%F %T') prune: target/debug is ${used_gb} GB, under the ${LIMIT_GB} GB limit, kept"
  exit 0
fi
echo "$(date '+%F %T') prune: target/debug is ${used_gb} GB, over the ${LIMIT_GB} GB limit, deleting"
rm -rf "$DEBUG"
echo "$(date '+%F %T') prune: freed about ${used_gb} GB"
