#!/bin/bash
# Rate-limiter bucket-map growth leg: one real `daiorama serve` process, real unix-socket
# frames, and a caller that chooses its own session id. Since #117 an id that cannot name a
# session is never keyed, so this leg's growth ids (longer than the 128-byte id bound) must
# leave the bucket map flat; it measures the manager's VmRSS across rounds of such ids, and
# [D] paces a seeded closed session (`known_session`).
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
    "$ROOT/config/daiorama/default/scopes/operator.ron" \
    "$ROOT/data/daiorama/default"
RC=$?
echo "ratebuckets_rc=$RC"
if [ "$RC" -eq 0 ]; then
    echo "VERDICT=PASS"
else
    echo "VERDICT=FAIL"
fi
exit "$RC"
