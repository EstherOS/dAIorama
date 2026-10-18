#!/usr/bin/env bash
# Input tools, live: the shipped binary's act half against a real compositor.
#
# The unit tests cover the keysym table and the refusals; this leg covers the part they
# cannot: a real `daiorama serve` spawning a real mutter->Hyprland session, and the nine
# input tools driven through the shipped harness entry (`daiorama mcp`, stdio, scope in
# DAIORAMA_SCOPE). The observation is compositor-side, not the tool's own return value:
# `hyprctl cursorpos` for the pointer and a terminal window title for the keyboard.
#
# Runs in the L1 image (privileged, /dev/dri) under dbus-run-session, with the binary
# built from the tip under test at /out/daiorama-input and this file mounted at /src.
#
# Assertions: FAILS=<n> at the end; every [OK] is one observed fact.
set -u

OUT=${OUT:-/out}
DAIO=${DAIO:-/out/daiorama-input}
PROBE=${PROBE:-/src/p5/input/live/probe-input-live.py}
INSTANCE=${INSTANCE:-inp}
SCOPE=act
FAILS=0

ok()   { echo "[OK]   $*"; }
fail() { echo "[FAIL] $*"; FAILS=$((FAILS + 1)); }
note() { echo "[NOTE] $*"; }
step() { echo; echo "== $* =="; }

export HOME=/root
export XDG_RUNTIME_DIR=/tmp/xdg
export XDG_CONFIG_HOME=/root/.config
export XDG_DATA_HOME=/root/.local/share
export XDG_STATE_HOME=/root/.local/state
export PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin
install -d -m 700 "$XDG_RUNTIME_DIR" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME"

step "environment"
"$DAIO" version || { echo "binary missing at $DAIO"; exit 1; }
python3 -V 2>/dev/null || {
  apt-get update -qq >/dev/null 2>&1
  apt-get install -y -qq python3 >/dev/null 2>&1
}
python3 -V && ok "python3 for the stdio client" || { echo "no python3"; exit 1; }

step "instance: config setup (core config + operator scope + secrets)"
"$DAIO" config setup || fail "config setup"
SCOPES=$(dirname "$(find "$XDG_CONFIG_HOME" -name operator.ron | head -1)")
note "scope dir: ${SCOPES:-<none>}"
[ -n "${SCOPES:-}" ] && ok "config setup wrote a scope file" || fail "no operator.ron after setup"

# A scope that grants the act half. Deny-by-default, so the grants are the point: a
# bridge driven as this scope can call input/capture/query tools and nothing else.
cat > "$SCOPES/$SCOPE.ron" <<'RON'
(
  sessions: (visibility: all),
  limits: (rate: (per_sec: 20, burst: 40, mode: pace, max_wait_ms: 30000, max_queued: 64)),
  audit: (store_clips: false, store_screenshots: false),
  grant: (categories: ["input", "capture", "query"], tools: []),
)
RON
chmod 600 "$SCOPES/$SCOPE.ron"
# Second run mints a scope secret for the file that appeared after the first run.
"$DAIO" config setup >/dev/null || true
"$DAIO" config validate | tail -3
"$DAIO" config set limits.mode off || fail "config set limits.mode off"
grep -q 'mode: off' "$(find "$XDG_CONFIG_HOME" -name config.ron | head -1)" \
  && ok "rate limits off in the starter config" || note "limits.mode not visible in config.ron"

step "manager: serve"
"$DAIO" serve > /tmp/serve.log 2>&1 &
SERVE_PID=$!
RUNNING=0
for _ in $(seq 1 60); do
  if "$DAIO" status 2>/dev/null | grep -qi running; then RUNNING=1; break; fi
  sleep 1
done
[ "$RUNNING" = 1 ] && ok "manager running (pid $SERVE_PID)" || fail "manager never reported running: $(tail -3 /tmp/serve.log)"
"$DAIO" status | head -12

step "session 1: 1280x720 @ scale 1"
SID1=$("$DAIO" new --resolution 1280x720 --scale 1 --label input-live 2>&1 | tail -1 | tr -d '\r')
note "session id: $SID1"
UP=0
for _ in $(seq 1 90); do
  if "$DAIO" status "$SID1" 2>/dev/null | grep -qi running; then UP=1; break; fi
  sleep 1
done
[ "$UP" = 1 ] && ok "session 1 running" || fail "session 1 never reached running: $("$DAIO" status "$SID1" 2>&1 | tail -3)"

RT1=$(ls -dt /tmp/dr-* 2>/dev/null | head -1)
SIG1=$(ls "$RT1/hypr" 2>/dev/null | head -1)
WS1=$(ls "$RT1" | grep '^wayland-' | grep -v '^wayland-m$' | head -1)
note "rt=$RT1 sig=$SIG1 wayland=$WS1"
[ -n "$SIG1" ] && [ -n "$WS1" ] && ok "session 1 compositor socket and instance" \
  || fail "could not find session 1 compositor (rt=$RT1)"

cursor() { XDG_RUNTIME_DIR="$RT1" HYPRLAND_INSTANCE_SIGNATURE="$SIG1" hyprctl cursorpos 2>&1 | tail -1; }
C0=$(cursor)
note "cursorpos baseline: $C0"
echo "$C0" | grep -qE '^-?[0-9]+, -?[0-9]+$' && ok "hyprctl answers cursorpos" || fail "cursorpos unreadable: $C0"

# One assertion helper: run an MCP call, then read the compositor's own view.
move_and_expect() {
  local x=$1 y=$2 want_x=$3 want_y=$4 label=$5
  local before after
  before=$(cursor)
  if ! python3 "$PROBE" "$DAIO" "$SCOPE" pointer_move "{\"session_id\": \"$SID1\", \"x\": $x, \"y\": $y}" > /tmp/mcp-move.log 2>&1; then
    fail "$label: pointer_move refused"; tail -3 /tmp/mcp-move.log; return
  fi
  sleep 0.5
  after=$(cursor)
  echo "[NOTE] $label: cursorpos $before -> $after (asked $x,$y)"
  local ax ay
  ax=$(echo "$after" | cut -d, -f1 | tr -d ' ')
  ay=$(echo "$after" | cut -d, -f2 | tr -d ' ')
  if [ -n "$ax" ] && [ -n "$ay" ] && \
     [ $(( ax > want_x ? ax - want_x : want_x - ax )) -le 2 ] && \
     [ $(( ay > want_y ? ay - want_y : want_y - ay )) -le 2 ]; then
    ok "$label: compositor cursor at $after"
  else
    fail "$label: wanted ~$want_x,$want_y, compositor says $after"
  fi
}

move_and_expect 300 200 300 200 "pointer_move 300,200 (scale 1)"
move_and_expect 900 600 900 600 "pointer_move 900,600 (scale 1)"
move_and_expect 1 1 1 1 "pointer_move 1,1 (corner)"

step "keyboard: type_text + key_tap + one-character names into a real terminal"
# foot runs `cat` on a raw tty, so every byte the compositor delivers lands in a file:
# the assertion is the client's received input, not the tool's return value.
XDG_RUNTIME_DIR="$RT1" WAYLAND_DISPLAY="$WS1" \
  foot --app-id=dai-input-live sh -c 'stty -icanon min 1 -echo; cat > /tmp/got.txt' \
  > /tmp/foot.log 2>&1 &
FOOT_PID=$!
sleep 3
XDG_RUNTIME_DIR="$RT1" HYPRLAND_INSTANCE_SIGNATURE="$SIG1" hyprctl clients -j > /tmp/clients.json 2>&1
python3 - <<'MAPPED' && ok "foot window mapped in the session" || { fail "foot window never mapped"; tail -3 /tmp/foot.log; }
import json
clients = json.load(open('/tmp/clients.json'))
assert any('dai-input-live' in (c.get('class') or '') for c in clients), clients
MAPPED
XDG_RUNTIME_DIR="$RT1" HYPRLAND_INSTANCE_SIGNATURE="$SIG1" hyprctl dispatch focuswindow class:dai-input-live >/dev/null
sleep 0.5

# Byte-exact comparison: shell command substitution strips trailing newlines, which is exactly
# the byte a Return tap delivers -- compare the file with `cmp` against the expected bytes.
expect_bytes() { # expected-printf-b string  label
  local want=$1 label=$2 i
  printf '%b' "$want" > /tmp/want.txt
  for i in $(seq 1 40); do
    cmp -s /tmp/got.txt /tmp/want.txt && { ok "$label: client received $(od -An -c /tmp/want.txt | tr -s ' ')"; return; }
    sleep 0.1
  done
  fail "$label: wanted $(od -An -c /tmp/want.txt | tr -s ' '), client has $(od -An -c /tmp/got.txt | tr -s ' ')"
}

if python3 "$PROBE" "$DAIO" "$SCOPE" type_text "{\"session_id\": \"$SID1\", \"text\": \"Hello, 42!\"}" > /tmp/mcp-text.log 2>&1; then
  expect_bytes "Hello, 42!" "type_text Hello, 42! (first character and every shifted one)"
else
  fail "type_text refused"; tail -3 /tmp/mcp-text.log
fi

if python3 "$PROBE" "$DAIO" "$SCOPE" key_tap "{\"session_id\": \"$SID1\", \"key\": \"Return\"}" > /tmp/mcp-key.log 2>&1; then
  expect_bytes "Hello, 42!\n" "key_tap Return (a one-pair call reaches the client)"
else
  fail "key_tap Return refused"; tail -3 /tmp/mcp-key.log
fi

if python3 "$PROBE" "$DAIO" "$SCOPE" key_tap "{\"session_id\": \"$SID1\", \"key\": \"a\"}" > /tmp/mcp-key2.log 2>&1; then
  expect_bytes "Hello, 42!\na" "key_tap a (one-character keysym name)"
else
  fail "key_tap a refused: $(tail -2 /tmp/mcp-key2.log)"
fi

if python3 "$PROBE" "$DAIO" "$SCOPE" key_tap "{\"session_id\": \"$SID1\", \"key\": \"X\"}" > /tmp/mcp-key3.log 2>&1; then
  expect_bytes "Hello, 42!\naX" "key_tap X (shifted keysym name arrives shifted)"
else
  fail "key_tap X refused: $(tail -2 /tmp/mcp-key3.log)"
fi

step "pointer button + scroll answer ok (no client target to observe)"
if python3 "$PROBE" "$DAIO" "$SCOPE" pointer_click \
     "{\"session_id\": \"$SID1\", \"button\": \"left\"}" > /tmp/mcp-click.log 2>&1; then
  ok "pointer_click answered ok"
else
  fail "pointer_click refused"; tail -3 /tmp/mcp-click.log
fi
if python3 "$PROBE" "$DAIO" "$SCOPE" pointer_scroll \
     "{\"session_id\": \"$SID1\", \"dx\": 0.0, \"dy\": 2.0}" > /tmp/mcp-scroll.log 2>&1; then
  ok "pointer_scroll answered ok"
else
  fail "pointer_scroll refused"; tail -3 /tmp/mcp-scroll.log
fi
if python3 "$PROBE" "$DAIO" "$SCOPE" pointer_button_down \
     "{\"session_id\": \"$SID1\", \"button\": \"left\"}" > /tmp/mcp-down.log 2>&1; then
  ok "pointer_button_down answered ok (drag primitive half)"
else
  fail "pointer_button_down refused"; tail -3 /tmp/mcp-down.log
fi
if python3 "$PROBE" "$DAIO" "$SCOPE" pointer_button_up \
     "{\"session_id\": \"$SID1\", \"button\": \"left\"}" > /tmp/mcp-up.log 2>&1; then
  ok "pointer_button_up answered ok (drag primitive half)"
else
  fail "pointer_button_up refused"; tail -3 /tmp/mcp-up.log
fi

step "session 2: 1280x720 @ scale 2 -- screenshot pixels vs logical space"
SID2=$("$DAIO" new --resolution 1280x720 --scale 2 --label input-live-2 2>&1 | tail -1 | tr -d '\r')
note "session id: $SID2"
UP=0
for _ in $(seq 1 90); do
  if "$DAIO" status "$SID2" 2>/dev/null | grep -qi running; then UP=1; break; fi
  sleep 1
done
[ "$UP" = 1 ] && ok "session 2 running" || fail "session 2 never reached running: $("$DAIO" status "$SID2" 2>&1 | tail -3)"
RT2=$(ls -dt /tmp/dr-* 2>/dev/null | head -1)
SIG2=$(ls "$RT2/hypr" 2>/dev/null | head -1)
note "rt=$RT2 sig=$SIG2"
if [ -n "$SIG2" ]; then
  before=$(XDG_RUNTIME_DIR="$RT2" HYPRLAND_INSTANCE_SIGNATURE="$SIG2" hyprctl cursorpos 2>&1 | tail -1)
  if python3 "$PROBE" "$DAIO" "$SCOPE" pointer_move "{\"session_id\": \"$SID2\", \"x\": 300, \"y\": 200}" > /tmp/mcp-move2.log 2>&1; then
    sleep 0.5
    after=$(XDG_RUNTIME_DIR="$RT2" HYPRLAND_INSTANCE_SIGNATURE="$SIG2" hyprctl cursorpos 2>&1 | tail -1)
    echo "[NOTE] scale 2: cursorpos $before -> $after (asked 300,200 screenshot pixels = 150,100 logical)"
    ax=$(echo "$after" | cut -d, -f1 | tr -d ' '); ay=$(echo "$after" | cut -d, -f2 | tr -d ' ')
    if [ -n "$ax" ] && [ -n "$ay" ] && [ "$ax" -ge 148 ] && [ "$ax" -le 152 ] && [ "$ay" -ge 98 ] && [ "$ay" -le 102 ]; then
      ok "scale 2 conversion: compositor cursor at $after"
    else
      fail "scale 2 conversion: wanted ~150,100, compositor says $after"
    fi
  else
    fail "scale 2 pointer_move refused"; tail -3 /tmp/mcp-move2.log
  fi
else
  fail "no compositor socket for session 2"
fi

step "teardown"
"$DAIO" daemon end --force 2>&1 | tail -3
sleep 1
kill "$FOOT_PID" 2>/dev/null
LEFT=$(ls -d /tmp/dr-* 2>/dev/null | wc -l)
note "session runtime dirs left: $LEFT"

echo
echo "FAILS=$FAILS"
