set -u
DAIO=${DAIO:-/out/daiorama-input}
PROBE=${PROBE:-/out/pixel-probe.py}
export HOME=/root XDG_RUNTIME_DIR=/tmp/xdg XDG_CONFIG_HOME=/root/.config XDG_DATA_HOME=/root/.local/share
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
"$DAIO" serve >/tmp/serve.log 2>&1 &
for _ in $(seq 1 60); do "$DAIO" status 2>/dev/null | grep -qi running && break; sleep 1; done

fails=0
run() {
  label=$1; res=$2; scale=$3
  echo "== $label: $res @ scale $scale =="
  sid=$("$DAIO" new --resolution "$res" --scale "$scale" --label "pixel-$label" 2>&1 | tail -1 | tr -d '\r')
  for _ in $(seq 1 90); do "$DAIO" status "$sid" 2>/dev/null | grep -qi running && break; sleep 1; done
  rt=$(ls -dt /tmp/dr-* | head -1)
  ws=$(ls "$rt" | grep '^wayland-' | grep -v '^wayland-m$' | head -1)
  sig=$(ls "$rt/hypr" | head -1)
  echo "sid=$sid rt=$rt ws=$ws sig=$sig"
  XDG_RUNTIME_DIR="$rt" WAYLAND_DISPLAY="$ws" HYPRLAND_INSTANCE_SIGNATURE="$sig" \
    swaybg -c '#ff0000' >/tmp/wall-$label.log 2>&1 &
  fp=$!
  sleep 2
  if python3 "$PROBE" "$DAIO" act "$sid" "/tmp/shot-$label.png" >/tmp/pixel-$label.log 2>&1; then
    cat /tmp/pixel-$label.log
  else
    cat /tmp/pixel-$label.log
    fails=$((fails + 1))
  fi
  dim=$(sed -n 's/^DIM=//p' /tmp/pixel-$label.log | head -1)
  w=${dim%x*}; h=${dim#*x}
  ew=$(( ${res%x*} * scale )); eh=$(( ${res#*x} * scale ))
  if [ "$w" = "$ew" ] && [ "$h" = "$eh" ]; then
    echo "DIM_EXPECTED=$ew x $eh"
  else
    echo "DIM_MISMATCH got $dim, expected ${ew}x${eh}"
    fails=$((fails + 1))
  fi
  kill "$fp" 2>/dev/null || true
  "$DAIO" close "$sid" >/dev/null 2>&1 || true
}

run one 1280x720 1
run two 1920x1080 1
run three 1280x720 2
"$DAIO" daemon end --force >/dev/null 2>&1 || true
echo "PIXEL_FAILS=$fails"
[ "$fails" -eq 0 ] && echo VERDICT=PASS || echo VERDICT=FAIL
exit "$fails"
