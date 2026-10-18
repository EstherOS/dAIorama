#!/bin/bash
# History-state leg: invented ids must leave no directory and no unbounded cache behind.
# Usage: run-history-bound.sh <binary> <label>
set -u
BIN=$(readlink -f "$1")
LABEL=${2:-run}
ROOT=/tmp/dr-historybound-$LABEL
rm -rf "$ROOT"
mkdir -p "$ROOT"/config/daiorama/default/scopes "$ROOT"/data "$ROOT"/run
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
python3 "$(dirname "$0")/probe-history-bound.py" \
    "$ROOT/run/daiorama/default/manager.sock" \
    "$SERVE" \
    "$ROOT/data/daiorama/default/history"
RC=$?
echo "historybound_rc=$RC"
[ "$RC" -eq 0 ] && echo "VERDICT=PASS" || echo "VERDICT=FAIL"
exit "$RC"
