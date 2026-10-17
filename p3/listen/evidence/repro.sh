#!/usr/bin/env bash
# Network bridge live drill (P3 W1b): a real manager, a real `daiorama mcp --listen`, and raw
# HTTP requests — the gate matrix plus one relayed call on each side of it.
#
#   p3/listen/evidence/repro.sh | tee p3/listen/evidence/network-live.txt
#
# Needs no compositor: the manager runs off-mode (`limits.mode = off`) and these calls never
# create a session. Port 18757 is fixed so the raw transcript is readable.
set -u

BIN=${DAIORAMA_BIN:-"$(cd "$(dirname "$0")/../../.." && pwd)/target/debug/daiorama"}
ROOT=$(mktemp -d /tmp/dlnet.XXXXXX)
PORT=${PORT:-18757}

export HOME="$ROOT"
export XDG_CONFIG_HOME="$ROOT/config"
export XDG_DATA_HOME="$ROOT/data"
export XDG_RUNTIME_DIR="$ROOT/run"
export TMPDIR="$ROOT/tmp"
mkdir -p "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_RUNTIME_DIR" "$TMPDIR"
unset DAIORAMA_TOKEN DAIORAMA_SCOPE DAIORAMA_SCOPE_TOKEN

cleanup() {
    [ -n "${BRIDGE_PID:-}" ] && kill "$BRIDGE_PID" 2>/dev/null
    [ -n "${MANAGER_PID:-}" ] && kill "$MANAGER_PID" 2>/dev/null
    wait 2>/dev/null
    rm -rf "$ROOT"
}
trap cleanup EXIT

step() { printf '\n=== %s ===\n' "$*"; }

step "bin: $BIN"
"$BIN" --version

step "config setup"
"$BIN" config setup
"$BIN" config set limits.mode off

step "manager: serve (off-mode)"
"$BIN" serve >"$ROOT/manager.log" 2>&1 &
MANAGER_PID=$!
for _ in $(seq 1 50); do
    [ -S "$ROOT/run/daiorama/default/manager.sock" ] && break
    sleep 0.1
done
"$BIN" status --json >/dev/null || { cat "$ROOT/manager.log"; exit 1; }
echo "manager up (pid $MANAGER_PID)"

step "bridge: mcp --listen 127.0.0.1:$PORT"
"$BIN" mcp --listen "127.0.0.1:$PORT" >"$ROOT/bridge.log" 2>&1 &
BRIDGE_PID=$!
for _ in $(seq 1 50); do
    grep -q "listening on" "$ROOT/bridge.log" 2>/dev/null && break
    sleep 0.1
done
cat "$ROOT/bridge.log"

TOKEN=$(cat "$XDG_DATA_HOME/daiorama/default/token")
SECRET=$(cat "$XDG_DATA_HOME/daiorama/default/scope-tokens/operator")
URL="http://127.0.0.1:$PORT/mcp"

post() {
    local label=$1 body=$2
    shift 2
    step "$label"
    curl -sS -X POST "$URL" -H 'Content-Type: application/json' "$@" \
        -d "$body" -w '\n[HTTP %{http_code}]\n'
}

post "gate: no token" '{"jsonrpc":"2.0","id":1,"method":"initialize"}'
post "gate: token, no scope" '{"jsonrpc":"2.0","id":2,"method":"initialize"}' \
    -H "Daiorama-Token: $TOKEN"
post "gate: token + scope, no secret" '{"jsonrpc":"2.0","id":3,"method":"initialize"}' \
    -H "Daiorama-Token: $TOKEN" -H 'Daiorama-Scope: operator'
post "gate: wrong secret" '{"jsonrpc":"2.0","id":4,"method":"initialize"}' \
    -H "Daiorama-Token: $TOKEN" -H 'Daiorama-Scope: operator' -H 'Daiorama-Scope-Token: wrong'
post "origin mismatch" '{"jsonrpc":"2.0","id":5,"method":"initialize"}' \
    -H "Daiorama-Token: $TOKEN" -H 'Daiorama-Scope: operator' \
    -H "Daiorama-Scope-Token: $SECRET" -H 'Origin: http://evil.example'
step "GET /mcp"
curl -sS "$URL" -H "Daiorama-Token: $TOKEN" -w '\n[HTTP %{http_code}]\n'

CREDS=(-H "Daiorama-Token: $TOKEN" -H 'Daiorama-Scope: operator' -H "Daiorama-Scope-Token: $SECRET")
post "authenticated: initialize" '{"jsonrpc":"2.0","id":6,"method":"initialize"}' "${CREDS[@]}"
post "authenticated: tools/list" '{"jsonrpc":"2.0","id":7,"method":"tools/list"}' "${CREDS[@]}"
post "relay: list_capabilities (manager live count)" \
    '{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"list_capabilities"}}' "${CREDS[@]}"
post "relay: list_sessions" \
    '{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"list_sessions"}}' "${CREDS[@]}"
post "relay: unknown tool" \
    '{"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"nope"}}' "${CREDS[@]}"
post "notification (no id): accepted, never executed" \
    '{"jsonrpc":"2.0","method":"tools/call","params":{"name":"create_session"}}' "${CREDS[@]}"

step "after: session count unchanged"
"$BIN" list --json

step "done"
