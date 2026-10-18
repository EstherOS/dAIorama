#!/bin/bash
# B9 rate-limit live leg: one real `daiorama serve` process, real unix-socket frames, and
# a per-scope `limits.rate` file rewritten between calls (scope files are read per call).
#
# No compositor and no live session are needed: the bucket gate runs in `dispatch()` before
# any session lookup, and since #117/#118 a token is spent only for an id that can name a
# session. The probe therefore seeds one `session.created` line per id (`known_session`) --
# a session that closed earlier. The handler still answers `session_not_found`; what this leg
# measures is the gate in front of it.
#
# Usage: run-rate-limit.sh <binary> <label>
set -u
BIN=$(readlink -f "$1")
LABEL=${2:-run}
ROOT=/tmp/dr-rate-$LABEL
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

python3 "$(dirname "$0")/probe-rate-limit.py" \
    "$ROOT/run/daiorama/default/manager.sock" \
    "$ROOT/config/daiorama/default/scopes/operator.ron" \
    "$ROOT/data/daiorama/default"
RC=$?
echo "rate_rc=$RC"
if [ "$RC" -eq 0 ]; then
    echo "VERDICT=PASS"
else
    echo "VERDICT=FAIL"
fi
exit "$RC"
