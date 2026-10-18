#!/usr/bin/env bash
# First-key measurement, run against a pre-fix build and the fixed build.
#
# The act path opens one Wayland connection per call, uploads the keymap and sends keys on it.
# Against the real chain the compositor applies that keymap asynchronously, so a key sent in
# the same dispatch as the keymap is translated against the not-yet-installed map and dropped:
# every call loses its first press/release pair. On a pre-fix build this script shows
# `abcdef` -> `bcdef` (first character gone) and a one-pair `key_tap Return` delivering nothing.
#
# Usage: DAIO=/out/daiorama-base|/out/daiorama-input probe-firstkey.sh
set -u
DAIO=${DAIO:-/out/daiorama-input}; PROBE=${PROBE:-/src/p5/input/live/probe-input-live.py}
export HOME=/root XDG_RUNTIME_DIR=/tmp/xdg XDG_CONFIG_HOME=/root/.config XDG_DATA_HOME=/root/.local/share
export PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin
install -d -m 700 "$XDG_RUNTIME_DIR" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME"
"$DAIO" config setup >/dev/null 2>&1
SCOPES=$(dirname "$(find "$XDG_CONFIG_HOME" -name operator.ron | head -1)")
cat > "$SCOPES/act.ron" <<'RON'
(
  sessions: (visibility: all),
  limits: (rate: (per_sec: 20, burst: 40, mode: pace, max_wait_ms: 30000, max_queued: 64)),
  audit: (store_clips: false, store_screenshots: false),
  grant: (categories: ["input", "capture", "query"], tools: []),
)
RON
chmod 600 "$SCOPES/act.ron"
"$DAIO" config setup >/dev/null 2>&1
"$DAIO" config set limits.mode off >/dev/null 2>&1
"$DAIO" serve > /tmp/serve.log 2>&1 &
for _ in $(seq 1 60); do "$DAIO" status 2>/dev/null | grep -qi running && break; sleep 1; done
SID=$("$DAIO" new --resolution 1280x720 --scale 1 --label firstkey 2>&1 | tail -1 | tr -d '\r')
for _ in $(seq 1 90); do "$DAIO" status "$SID" 2>/dev/null | grep -qi running && break; sleep 1; done
RT=$(ls -dt /tmp/dr-* | head -1); SIG=$(ls "$RT/hypr" | head -1)
WS=$(ls "$RT" | grep '^wayland-' | grep -v '^wayland-m$' | head -1)
XDG_RUNTIME_DIR="$RT" WAYLAND_DISPLAY="$WS" \
  foot --app-id=dai-input-live sh -c 'stty -icanon min 1 -echo; cat > /tmp/got.txt' > /tmp/foot.log 2>&1 &
sleep 3
XDG_RUNTIME_DIR="$RT" HYPRLAND_INSTANCE_SIGNATURE="$SIG" hyprctl dispatch focuswindow class:dai-input-live >/dev/null
sleep 0.5
got(){ cat /tmp/got.txt 2>/dev/null; }

echo "binary: $DAIO"
echo "session: $SID"
echo
echo "== type_text abcdef, twice (six characters each) =="
for i in 1 2; do
  before=$(got)
  python3 "$PROBE" "$DAIO" act type_text "{\"session_id\": \"$SID\", \"text\": \"abcdef\"}" >/tmp/mcp.log 2>&1
  sleep 0.8
  after=$(got)
  echo "call $i: delta=$(printf '%q' "${after#$before}")"
done
echo
echo "== key_tap Return, twice (one press/release pair each) =="
for i in 1 2; do
  before=$(got)
  python3 "$PROBE" "$DAIO" act key_tap "{\"session_id\": \"$SID\", \"key\": \"Return\"}" >/tmp/mcp.log 2>&1
  sleep 0.8
  after=$(got)
  echo "tap $i: delta=$(printf '%q' "${after#$before}")"
done
echo
echo "== every byte the client received =="
od -c /tmp/got.txt | head -4
"$DAIO" daemon end --force >/dev/null 2>&1
echo RUN-DONE
