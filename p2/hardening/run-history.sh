#!/bin/bash
# History live leg: one real `daiorama serve` process, a real unix socket, records written by
# `dispatch()` for control-class calls, and `list_history` read back over the same socket.
#
# No compositor and no live session are needed: `dispatch()` records `tool.call` for the nine
# control-class tools before any session lookup, but since #118 only for an id that can name a
# session. The probe seeds one `session.created` line per id (`known_session`) -- a session
# that closed earlier -- and filters that line out of its `tool.call` counts.
#
# Usage: run-history.sh <binary> <label>
set -u
BIN=$(readlink -f "$1")
LABEL=${2:-run}
ROOT=/tmp/dr-history-$LABEL
rm -rf "$ROOT"
mkdir -p "$ROOT"/config/daiorama/default "$ROOT"/data "$ROOT"/run
printf '(version: 1, limits: (mode: off))\n' > "$ROOT"/config/daiorama/default/config.ron

export XDG_CONFIG_HOME="$ROOT/config"
export XDG_DATA_HOME="$ROOT/data"
export XDG_RUNTIME_DIR="$ROOT/run"
export TMPDIR="$ROOT"

"$BIN" serve > "$ROOT/serve.log" 2>&1 &
SERVE=$!
trap 'kill $SERVE 2>/dev/null; wait $SERVE 2>/dev/null' EXIT

for _ in $(seq 1 100); do
    [ -S "$ROOT/run/daiorama/default/manager.sock" ] && break
    sleep 0.1
done
if [ ! -S "$ROOT/run/daiorama/default/manager.sock" ]; then
    echo "manager socket never appeared; serve.log:"
    tail -20 "$ROOT/serve.log"
    exit 90
fi

python3 "$(dirname "$0")/probe-history.py" \
    "$ROOT/run/daiorama/default/manager.sock" \
    "$ROOT/data/daiorama/default"
