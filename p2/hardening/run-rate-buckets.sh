#!/bin/bash
# Rate-limiter bucket-map growth leg: one real `daiorama serve` process, real unix-socket
# frames, and a caller that chooses its own session id. `dispatch()` runs the control-class
# gate before any session lookup, so ids that name nothing still reach the bucket map; this
# leg measures the manager's VmRSS across a burst of unique ids.
#
# Usage: run-rate-buckets.sh <binary> <label>
set -u
BIN=$(readlink -f "$1")
LABEL=${2:-run}
ROOT=/tmp/dr-ratebuckets-$LABEL
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

python3 "$(dirname "$0")/probe-rate-buckets.py" \
    "$ROOT/run/daiorama/default/manager.sock" \
    "$SERVE" \
    "$ROOT/config/daiorama/default/scopes/operator.ron"
RC=$?
echo "ratebuckets_rc=$RC"
if [ "$RC" -eq 0 ]; then
    echo "VERDICT=PASS"
else
    echo "VERDICT=FAIL"
fi
exit "$RC"
