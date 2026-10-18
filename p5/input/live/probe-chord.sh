#!/usr/bin/env bash
# Measurement, not a gate: does 05's chord primitive (modifier down -> tap -> modifier up)
# survive the per-call connection? Each call opens and drops its own Wayland device, so the
# modifier state a `key_down` sets lives exactly as long as that one call.
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
SID=$("$DAIO" new --resolution 1280x720 --scale 1 --label chord 2>&1 | tail -1 | tr -d '\r')
for _ in $(seq 1 90); do "$DAIO" status "$SID" 2>/dev/null | grep -qi running && break; sleep 1; done
RT=$(ls -dt /tmp/dr-* | head -1); SIG=$(ls "$RT/hypr" | head -1)
WS=$(ls "$RT" | grep '^wayland-' | grep -v '^wayland-m$' | head -1)
XDG_RUNTIME_DIR="$RT" WAYLAND_DISPLAY="$WS" \
  foot --app-id=dai-input-live sh -c 'stty -icanon min 1 -echo; cat > /tmp/got.txt' > /tmp/foot.log 2>&1 &
sleep 3
XDG_RUNTIME_DIR="$RT" HYPRLAND_INSTANCE_SIGNATURE="$SIG" hyprctl dispatch focuswindow class:dai-input-live >/dev/null
sleep 0.5
got(){ cat /tmp/got.txt 2>/dev/null; }
call(){ python3 "$PROBE" "$DAIO" act "$1" "$2" >/dev/null 2>&1; }

echo "== chord across calls: key_down Shift_L / key_tap x / key_up Shift_L =="
call key_down "{\"session_id\": \"$SID\", \"key\": \"Shift_L\"}"
call key_tap  "{\"session_id\": \"$SID\", \"key\": \"x\"}"
call key_up   "{\"session_id\": \"$SID\", \"key\": \"Shift_L\"}"
sleep 1
after_chord=$(got)
echo "client bytes: $(printf '%q' "$after_chord")"
case "$after_chord" in
  *X*) echo "CHORD=works (shift survived the calls)";;
  *x*) echo "CHORD=broken (tap arrived unshifted: the modifier died with its one-call device)";;
  *)   echo "CHORD=no-input (nothing arrived)";;
esac

echo
echo "== control: shift inside one call (key_tap of a shifted keysym name) =="
before=$(got)
call key_tap "{\"session_id\": \"$SID\", \"key\": \"X\"}"
sleep 1
echo "client bytes: $(printf '%q' "$(got)")"
case "$(got)" in
  "${before}X") echo "SHIFTED_TAP=works";;
  *) echo "SHIFTED_TAP=broken: wanted ${before}X";;
esac

"$DAIO" daemon end --force >/dev/null 2>&1
echo RUN-DONE
