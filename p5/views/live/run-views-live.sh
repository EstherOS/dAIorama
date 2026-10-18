#!/bin/bash
# Live leg: the view lifecycle as an operator actually hits it.
#
# Every view test so far starts one view and reads its answer. 06 pins more than that:
# zero-or-one per session with a serialized start path, a generation that moves on every
# start, ports released on stop/death, "view down != session dead", and — the promise this
# leg exists for — *a dead wayvnc is replaced by the next `get_view` with a new generation*.
# A real harness watches the same desktop from two clients, stops the view, restarts it, and
# sees wayvnc crash; the operator then reloads the page. This leg does exactly that against a
# real manager and a real Hyprland session.
#
# Runs inside the L1 image (Hyprland), under dbus-run-session.
# Usage: run-views-live.sh [binary]; env LABEL names the run.
set -u
DAIO=${1:-/out/daiorama}
HERE=$(dirname "$(readlink -f "$0")")
LABEL=${LABEL:-run}
ROOT=${ROOT:-/tmp/dr-views-$LABEL}
LOGS=${LOGS:-/out/logs-views-$LABEL}
rm -rf "$ROOT" "$LOGS"
mkdir -p "$ROOT/config/daiorama/default" "$ROOT/data" "$ROOT/run" "$LOGS"
chmod 700 "$ROOT/run"
export XDG_CONFIG_HOME="$ROOT/config" XDG_DATA_HOME="$ROOT/data" XDG_RUNTIME_DIR="$ROOT/run"
export XDG_SESSION_TYPE=wayland TMPDIR="$ROOT"

CONF=$XDG_CONFIG_HOME/daiorama/default
DATA=$XDG_DATA_HOME/daiorama/default
SERVE=""
fails=0
bad() { echo "[FAIL] $*"; fails=$((fails + 1)); }
good() { echo "[OK]   $*"; }
finish() {
    kill "$SERVE" 2>/dev/null
    pkill -x wayvnc 2>/dev/null
    cp -a "$ROOT"/*.log "$LOGS"/ 2>/dev/null
    cp -a "$CONF" "$LOGS"/config 2>/dev/null
}
trap finish EXIT

count_wayvnc() { ps -eo comm= 2>/dev/null | grep -cx wayvnc; }
# `ss`/`netstat` are not in the L1 image; a connect is the honest check anyway. curl exit 7 is
# "could not connect" — anything else (including 403/400/timeout) means something is listening.
port_open() { curl -s -o /dev/null --max-time 2 "http://127.0.0.1:$1/" 2>/dev/null; [ "$?" != "7" ]; }
wait_gone() { # wait_gone <port> <seconds>
    for _ in $(seq 1 $(( $2 * 10 ))); do
        port_open "$1" || return 0
        sleep 0.1
    done
    return 1
}
front_code() { curl -s -o /dev/null -w '%{http_code}' --max-time 3 "http://127.0.0.1:$1/?credential=$2" 2>/dev/null; }
answer_field() { python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))[sys.argv[2]])' "$1" "$2" 2>/dev/null; }
port_of() { sed -E 's#^http://[^:/]+:([0-9]+).*#\1#' <<<"$1"; }

cat > "$CONF/config.ron" <<'RON'
(
  version: 1,
  manager: (start_mode: auto, bind: None),
  limits: (mode: off),
)
RON

echo "== [A] a real manager and one live Hyprland session =="
"$DAIO" serve > "$ROOT/serve.log" 2>&1 &
SERVE=$!
for _ in $(seq 1 120); do "$DAIO" status --json 2>/dev/null | grep -q '"running"' && break; sleep 0.5; done
if "$DAIO" status --json 2>/dev/null | grep -q '"running"'; then good "the manager answers status"; else bad "manager status"; tail -5 "$ROOT/serve.log"; fi

SID=$("$DAIO" new --label views-leg --resolution 1280x720 --scale 1 2>"$ROOT/new.err" | tail -1 | tr -d '\r')
if [ -n "$SID" ]; then good "session created: $SID"; else bad "session create: $(tail -3 "$ROOT/new.err")"; fi
for _ in $(seq 1 180); do "$DAIO" status "$SID" --json 2>/dev/null | grep -q '"running"' && break; sleep 0.5; done
if "$DAIO" status "$SID" --json 2>/dev/null | grep -q '"running"'; then good "the session is running"; else bad "session did not reach running"; fi

# The front serves the noVNC tree; production stages it with packaging/novnc/fetch.sh.
if [ -d /usr/share/novnc ]; then
    cp -a /usr/share/novnc "$DATA/novnc" && good "staged $(find "$DATA/novnc" -type f | wc -l) view asset files"
else
    bad "no /usr/share/novnc in the image to stage"
fi

echo
echo "== [B] four clients ask for the view at once: one wayvnc, one front, one answer =="
PIDS=""
for i in 1 2 3 4; do
    timeout 30 "$DAIO" view "$SID" --json > "$ROOT/view-$i.json" 2>"$ROOT/view-$i.err" &
    PIDS="$PIDS $!"
done
# shellcheck disable=SC2086
wait $PIDS
GENS=""; ADDRS=""
for i in 1 2 3 4; do
    g=$(answer_field "$ROOT/view-$i.json" generation)
    a=$(answer_field "$ROOT/view-$i.json" address)
    [ -n "$g" ] || { bad "client $i got no generation: $(tail -2 "$ROOT/view-$i.err")"; continue; }
    GENS="$GENS $g"; ADDRS="$ADDRS $a"
    echo "  client $i: generation=$g port=$(port_of "$a")"
done
UNIQ_GENS=$(tr ' ' '\n' <<<"$GENS" | grep -c . )
if [ "$(tr ' ' '\n' <<<"$GENS" | grep . | sort -u | wc -l)" = "1" ]; then
    good "all four concurrent starts answered one generation$( [ "$UNIQ_GENS" -gt 0 ] && echo " ($(tr ' ' '\n' <<<"$GENS" | grep . | head -1))")"
else
    bad "concurrent starts answered different generations:$GENS"
fi
if [ "$(tr ' ' '\n' <<<"$ADDRS" | grep . | sort -u | wc -l)" = "1" ]; then
    good "all four answered one address"
else
    bad "concurrent starts answered different addresses:$ADDRS"
fi
GEN1=$(tr ' ' '\n' <<<"$GENS" | grep . | head -1)
ADDR1=$(tr ' ' '\n' <<<"$ADDRS" | grep . | head -1)
CRED1=$(answer_field "$ROOT/view-1.json" credential)
PORT1=$(port_of "$ADDR1")
N=$(count_wayvnc); if [ "$N" = "1" ]; then good "exactly one wayvnc process"; else bad "wayvnc processes: $N (want 1)"; fi
if port_open "$PORT1"; then good "something is listening on the view port $PORT1"; else bad "nothing listens on the view port $PORT1"; fi
CODE=$(front_code "$PORT1" "$CRED1")
if [ "$CODE" = "200" ]; then good "the front serves the page with the credential (HTTP 200)"; else bad "front with credential: HTTP $CODE"; fi
CODE=$(curl -s -o /dev/null -w '%{http_code}' --max-time 3 "http://127.0.0.1:$PORT1/" 2>/dev/null)
if [ "$CODE" = "403" ]; then good "the front refuses a page request without a credential (HTTP 403)"; else bad "front without credential: HTTP $CODE (want 403)"; fi

echo
echo "== [C] stop releases the front port and the wayvnc process =="
OUT=$("$DAIO" view "$SID" --stop 2>&1); RC=$?
if [ "$RC" = "0" ]; then good "view --stop exits 0"; else bad "view --stop rc=$RC: $OUT"; fi
if wait_gone "$PORT1" 3; then good "the front port $PORT1 is closed after stop"; else bad "the front port $PORT1 still listens 3 s after stop"; fi
N=$(count_wayvnc); if [ "$N" = "0" ]; then good "no wayvnc after stop"; else bad "wayvnc processes after stop: $N"; fi
"$DAIO" status "$SID" --json > "$ROOT/status-stop.json" 2>/dev/null
STATE=$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); v=d.get("view") or {}; print(v.get("state"))' "$ROOT/status-stop.json" 2>/dev/null)
if [ "$STATE" = "None" ] || [ "$STATE" = "null" ]; then good "session detail reports no view after stop"; else bad "session detail view state after stop: $STATE"; fi

echo
echo "== [D] the next get_view starts a fresh view and moves the generation =="
"$DAIO" view "$SID" --json > "$ROOT/view-restart.json" 2>"$ROOT/view-restart.err"
GEN2=$(answer_field "$ROOT/view-restart.json" generation)
ADDR2=$(answer_field "$ROOT/view-restart.json" address)
CRED2=$(answer_field "$ROOT/view-restart.json" credential)
PORT2=$(port_of "$ADDR2")
echo "  restart: generation=$GEN2 port=$PORT2"
if [ -n "$GEN2" ] && [ "$GEN2" -gt "${GEN1:-0}" ]; then good "generation moved $GEN1 -> $GEN2"; else bad "generation did not move on restart ($GEN1 -> $GEN2)"; fi
N=$(count_wayvnc); if [ "$N" = "1" ]; then good "a new wayvnc is running"; else bad "wayvnc processes after restart: $N"; fi
CODE=$(front_code "$PORT2" "$CRED2")
if [ "$CODE" = "200" ]; then good "the restarted front answers 200 on $PORT2"; else bad "restarted front: HTTP $CODE"; fi

echo
echo "== [E] wayvnc dies: the session lives, the view must not be handed back as live =="
BEFORE_STATE=$("$DAIO" status "$SID" --json 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin).get("state"))' 2>/dev/null)
pkill -9 -x wayvnc
sleep 1.5
AFTER_STATE=$("$DAIO" status "$SID" --json 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin).get("state"))' 2>/dev/null)
if [ "$AFTER_STATE" = "running" ]; then good "session stayed running after the view died ($BEFORE_STATE -> running)"; else bad "session state after view death: $AFTER_STATE (want running)"; fi
"$DAIO" status "$SID" --json > "$ROOT/status-dead.json" 2>/dev/null
VSTATE=$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); v=d.get("view") or {}; print(v.get("state"), v.get("generation"))' "$ROOT/status-dead.json" 2>/dev/null)
echo "  session detail after the crash: view=$VSTATE"
CODE=$(front_code "$PORT2" "$CRED2")
echo "  the dead view's front still answers: HTTP $CODE"
"$DAIO" view "$SID" --json > "$ROOT/view-after-death.json" 2>"$ROOT/view-after-death.err"
GEN3=$(answer_field "$ROOT/view-after-death.json" generation)
ADDR3=$(answer_field "$ROOT/view-after-death.json" address)
CRED3=$(answer_field "$ROOT/view-after-death.json" credential)
PORT3=$(port_of "$ADDR3")
echo "  get_view after the crash: generation=$GEN3 port=$PORT3"
if [ -n "$GEN3" ] && [ "$GEN3" -gt "${GEN2:-0}" ]; then
    good "a dead view is replaced: generation moved $GEN2 -> $GEN3"
    N=$(count_wayvnc); if [ "$N" = "1" ]; then good "the replacement wayvnc is running"; else bad "wayvnc processes after replacement: $N"; fi
    CODE=$(front_code "$PORT3" "$CRED3")
    if [ "$CODE" = "200" ]; then good "the replacement front answers 200 on $PORT3"; else bad "replacement front: HTTP $CODE"; fi
else
    bad "a dead view is handed back as live: generation stayed $GEN2 (front HTTP $CODE), so the operator's reload can never recover without a manual stop"
fi

echo
echo "== [F] closing the session takes the view with it =="
"$DAIO" view "$SID" --stop >/dev/null 2>&1
"$DAIO" close "$SID" >/dev/null 2>&1
N=$(count_wayvnc)
if [ "$N" = "0" ]; then good "no wayvnc after close"; else bad "wayvnc processes after close: $N"; fi
LEFT=0
for p in 6080 6081 6082; do port_open "$p" && LEFT=$((LEFT + 1)); done
if [ "$LEFT" = "0" ]; then good "no view port left listening"; else bad "view ports still listening after close: $LEFT"; fi

echo
echo "ASSERT_FAILS=$fails"
if [ "$fails" = "0" ]; then echo "VERDICT=PASS"; else echo "VERDICT=FAIL"; fi
exit "$fails"
