#!/bin/bash
# Live leg: the dashboard UI as an operator uses it — installed module, real manager, real Chromium.
#
# The shipped page (`dashboard/ui/app.js`) is untested at the browser level: every other
# dashboard leg speaks HTTP to the server. This leg installs the module from a real artifact,
# points it at a real manager holding a live session, loads the module's own page in Chromium and
# drives the DOM through CDP (`probe-ui.py`): the manager row, the session table, a row click, the
# history pane and Open view.
#
# Runs inside the L1 image (Hyprland + chromium), under dbus-run-session.
# Usage: run-dashboard-ui.sh [binary] [artifact]; env LABEL names the run.
set -u
DAIO=${1:-/out/daiorama}
ART=${2:-/out/daiorama-dashboard-0.1.0-x86_64-unknown-linux-gnu.tar.gz}
HERE=$(dirname "$(readlink -f "$0")")
LABEL=${LABEL:-run}
ROOT=${ROOT:-/tmp/dr-ui-$LABEL}
LOGS=${LOGS:-/out/logs-$LABEL}
rm -rf "$ROOT" "$LOGS"
mkdir -p "$ROOT/config/daiorama/default" "$ROOT/data" "$ROOT/run" "$LOGS"
chmod 700 "$ROOT/run"
export XDG_CONFIG_HOME="$ROOT/config" XDG_DATA_HOME="$ROOT/data" XDG_RUNTIME_DIR="$ROOT/run"
export XDG_SESSION_TYPE=wayland TMPDIR="$ROOT"

CONF=$XDG_CONFIG_HOME/daiorama/default
DATA=$XDG_DATA_HOME/daiorama/default
CHROME=""
DASH=""
SERVE=""
fails=0
bad() { echo "[FAIL] $*"; fails=$((fails + 1)); }
good() { echo "[OK]   $*"; }
finish() {
    kill "$CHROME" "$DASH" "$SERVE" 2>/dev/null
    cp -a "$ROOT"/*.log "$LOGS"/ 2>/dev/null
    cp -a "$ROOT/config/daiorama/default" "$LOGS"/config 2>/dev/null
}
trap finish EXIT

cat > "$CONF/config.ron" <<'RON'
(
  version: 1,
  manager: (start_mode: auto, bind: Some("127.0.0.1:8756")),
  limits: (mode: off),
)
RON

echo "== [A] the dashboard scope is created before the first network bind =="
if "$DAIO" config scope create dashboard --template dashboard; then good "config scope create dashboard"; else bad "config scope create dashboard"; fi

echo
echo "== [B] a real manager on manager.bind, with one live session =="
"$DAIO" serve > "$ROOT/serve.log" 2>&1 &
SERVE=$!
for _ in $(seq 1 120); do [ -s "$DATA/token" ] && [ -s "$DATA/scope-tokens/dashboard" ] && break; sleep 0.5; done
if [ -s "$DATA/token" ] && [ -s "$DATA/scope-tokens/dashboard" ]; then
    good "the TCP bind minted the instance token and the dashboard scope secret"
else
    bad "token/secret minting"; tail -5 "$ROOT/serve.log"
fi
for _ in $(seq 1 120); do "$DAIO" status --json 2>/dev/null | grep -q '"running"' && break; sleep 0.5; done
if "$DAIO" status --json 2>/dev/null | grep -q '"running"'; then good "the manager answers status"; else bad "manager status"; fi

SID=$("$DAIO" new --label ui-leg --resolution 1280x720 --scale 1 2>"$ROOT/new.err" | tail -1 | tr -d '\r')
if [ -n "$SID" ]; then good "session created: $SID"; else bad "session create: $(tail -3 "$ROOT/new.err")"; fi
for _ in $(seq 1 180); do "$DAIO" status "$SID" --json 2>/dev/null | grep -q '"running"' && break; sleep 0.5; done
if "$DAIO" status "$SID" --json 2>/dev/null | grep -q '"running"'; then good "the session is running"; else bad "session did not reach running"; fi

echo
echo "== [C] the module is installed from a real artifact and points at the manager =="
SHA=$(sha256sum "$ART" | cut -d' ' -f1)
echo "artifact=$(basename "$ART") sha256=$SHA"
INSTALLED=""
if OUT=$("$DAIO" dashboard install --artifact "$ART" --sha256 "$SHA"); then
    good "dashboard install"
    INSTALLED=$DATA/modules/daiorama-dashboard/0.1.0/daiorama-dashboard
else
    bad "dashboard install: $OUT"
fi
"$DAIO" dashboard connect 127.0.0.1:8756 || bad "dashboard connect"
printf '%s' "$(cat "$DATA/token")" | "$DAIO" dashboard token set 127.0.0.1:8756 || bad "token set"
# The scope secret is a module-local verb (04 grammar): the core forwards serve/connect/
# connections/token only, so the module binary the core installed is the right caller.
if [ -x "$INSTALLED" ]; then
    printf '%s' "$(cat "$DATA/scope-tokens/dashboard")" | "$INSTALLED" scope-token set 127.0.0.1:8756 && good "scope-token set" || bad "scope-token set"
else
    bad "installed module binary missing at $INSTALLED"
fi
"$DAIO" dashboard connections list

echo
echo "== [C2] the view asset tree the front serves (the installer's job in production) =="
# The L1 image ships Debian's noVNC tree; production stages it with `packaging/novnc/fetch.sh`.
# Staging it at `<data>/novnc` makes Open view land on a real page instead of the front's
# `view assets not installed` refusal, so the leg covers the last mile of the click-through.
if [ -d /usr/share/novnc ]; then
    cp -a /usr/share/novnc "$DATA/novnc" && good "staged $(find "$DATA/novnc" -type f | wc -l) asset files at <data>/novnc"
    [ -f "$DATA/novnc/vnc.html" ] && good "the front's entry point vnc.html is present" || bad "vnc.html missing from the staged tree"
else
    bad "no /usr/share/novnc in the image to stage"
fi

echo
echo "== [D] the installed module serves its own UI on loopback =="
"$DAIO" dashboard serve > "$ROOT/dash.log" 2>&1 &
DASH=$!
for _ in $(seq 1 120); do curl -fsS -o "$ROOT/index.html" "http://127.0.0.1:8760/" && break; sleep 0.5; done
if [ -s "$ROOT/index.html" ]; then
    good "GET / answered $(wc -c < "$ROOT/index.html") bytes"
else
    bad "the module's page never answered"; tail -5 "$ROOT/dash.log"
fi
if curl -fsS -o "$ROOT/app.js" "http://127.0.0.1:8760/app.js"; then
    good "GET /app.js answered $(wc -c < "$ROOT/app.js") bytes"
else
    bad "app.js not served"
fi
echo "GET /api/managers: $(curl -fsS http://127.0.0.1:8760/api/managers)"
echo "GET /api/sessions: $(curl -fsS "http://127.0.0.1:8760/api/sessions?manager=127.0.0.1:8756")"

echo
echo "== [E] the page runs in Chromium and the DOM is asserted =="
CHROME_BIN=$(command -v chromium || command -v chromium-browser || true)
echo "chromium=$CHROME_BIN"
"$CHROME_BIN" --headless=new --no-sandbox --disable-gpu --disable-dev-shm-usage \
    --user-data-dir="$ROOT/chrome" --remote-debugging-port=9222 --window-size=1280,900 \
    about:blank > "$ROOT/chrome.log" 2>&1 &
CHROME=$!
for _ in $(seq 1 120); do curl -fsS -o /dev/null "http://127.0.0.1:9222/json/version" 2>/dev/null && break; sleep 0.5; done
if curl -fsS -o /dev/null "http://127.0.0.1:9222/json/version"; then good "Chromium CDP is up"; else bad "Chromium CDP did not start"; tail -5 "$ROOT/chrome.log"; fi
python3 "$HERE/probe-ui.py" --port 8760 --session "$SID" --cdp 9222 | tee "$ROOT/probe.log"
PROBE=${PIPESTATUS[0]}
if [ "$PROBE" -eq 0 ]; then good "probe-ui.py PASS"; else bad "probe-ui.py exit $PROBE"; fi

echo
echo "== [E2] the URL the click opened actually serves the view front =="
OPENED=$(sed -n 's/^OPENED=//p' "$ROOT/probe.log" | tail -1)
if [ -n "$OPENED" ]; then
    CODE=$(curl -sS -o "$ROOT/view.html" -w '%{http_code}' "$OPENED")
    echo "view_front_http=$CODE bytes=$(wc -c < "$ROOT/view.html")"
    if [ "$CODE" = "200" ] && grep -qi 'novnc' "$ROOT/view.html"; then
        good "the opened address serves the real noVNC page"
    else
        bad "the opened address answered $CODE ($(head -c 120 "$ROOT/view.html" | tr -d '\n'))"
    fi
else
    bad "the page opened no view target"
fi

echo
echo "== [F] the page stayed a page: no crash, no reload loop =="
if curl -fsS -o /dev/null "http://127.0.0.1:8760/"; then good "the module still answers after the browser run"; else bad "module stopped answering"; fi
echo "chrome_severe_lines=$(grep -c 'SEVERE\|Uncaught' "$ROOT/chrome.log" 2>/dev/null || true)"

"$DAIO" close "$SID" >/dev/null 2>&1 || true
"$DAIO" daemon end --force >/dev/null 2>&1 || true

echo
echo "UI_FAILS=$fails"
[ "$fails" -eq 0 ] && echo VERDICT=PASS || echo VERDICT=FAIL
exit "$fails"
