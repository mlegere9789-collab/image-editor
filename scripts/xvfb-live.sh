#!/usr/bin/env bash
# Runs the real desktop app on a virtual X display for live verification
# (README Phase 362): a Vite dev server on port 1420 (a debug build's
# window loads `build.devUrl` from tauri.conf.json rather than embedded
# assets), an Xvfb started with `-ac` so screenshots and xdotool from
# another shell are not refused by X authorization, and the debug
# binary. Prints the display and window id, and leaves everything
# running; `scripts/xvfb-live.sh stop` ends it.
#
#   scripts/xvfb-live.sh            # start, print DISPLAY and window id
#   DISPLAY=:101 import -window root shot.png
#   DISPLAY=:101 xdotool mousemove 28 13 click 1
#   scripts/xvfb-live.sh stop
set -euo pipefail
cd "$(dirname "$0")/.."
DISPLAY_NUMBER="${DISPLAY_NUMBER:-:101}"
if [ "${1:-}" = "stop" ]; then
  pkill -x image-editor || true
  pkill -x Xvfb || true
  pkill -f "[v]ite --port 1420" || true
  exit 0
fi
if ! curl -s -o /dev/null "http://localhost:1420/"; then
  (npx vite --port 1420 --strictPort > /tmp/xvfb-live-vite.log 2>&1 &)
  for _ in $(seq 1 40); do curl -s -o /dev/null "http://localhost:1420/" && break; sleep 0.5; done
fi
if ! pgrep -x Xvfb > /dev/null; then
  (Xvfb "$DISPLAY_NUMBER" -screen 0 1400x900x24 -ac > /tmp/xvfb-live-x.log 2>&1 &)
  sleep 2
fi
[ -x src-tauri/target/debug/image-editor ] || (cd src-tauri && cargo build)
(DISPLAY="$DISPLAY_NUMBER" src-tauri/target/debug/image-editor > /tmp/xvfb-live-app.log 2>&1 &)
for _ in $(seq 1 60); do
  if W=$(DISPLAY="$DISPLAY_NUMBER" xdotool search --name "LegeLabs" 2>/dev/null | head -1) && [ -n "$W" ]; then
    echo "DISPLAY=$DISPLAY_NUMBER window=$W"
    exit 0
  fi
  sleep 0.5
done
echo "The app's window did not appear; see /tmp/xvfb-live-app.log" >&2
exit 1
