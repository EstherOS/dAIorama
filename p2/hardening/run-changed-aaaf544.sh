#!/bin/bash
# Changed-surface legs at aaaf544: the source that moved since the last full pass at dc59f6e is
# `src/runtime/rate.rs` (#117 bucket bounds), `src/history.rs` (#118 invented ids + bounded
# cache), `src/runtime/service.rs` (#117/#118 wiring) and `packaging/release/install.sh` (#116).
# This runner drives the five legs that own that surface (install.sh has its own leg,
# run-install-live.sh). Legs over unchanged surfaces are not re-run here.
# Usage: run-changed-aaaf544.sh <tag>
set -u
TAG=${1:-local}
cd /src || exit 1
export CARGO_TERM_COLOR=never
export CARGO_TARGET_DIR=/tmp/t
logdir=/src/p2/hardening/evidence

cargo build --workspace --bins --locked >/tmp/build.log 2>&1 || {
  echo "build failed"; tail -25 /tmp/build.log; exit 1; }
BIN=/tmp/t/debug/daiorama
echo "binary: $(ls -l "$BIN" | awk '{print $5}') bytes"

fails=0
runleg() { # runleg <name> <cmd...>
  local name=$1; shift
  "$@" >"$logdir/$name-$TAG.log" 2>&1
  local rc=$?
  echo "$name rc=$rc"
  [ "$rc" -ne 0 ] && fails=$((fails + 1))
  return 0
}

runleg dispatch-stall bash p2/hardening/run-dispatch-stall.sh "$BIN" "$TAG"
runleg rate-limit bash p2/hardening/run-rate-limit.sh "$BIN" "$TAG"
runleg rate-buckets bash p2/hardening/run-rate-buckets.sh "$BIN" "$TAG"
runleg history bash p2/hardening/run-history.sh "$BIN" "$TAG"
runleg history-bound bash p2/hardening/run-history-bound.sh "$BIN" "$TAG"

echo "LEG_FAILS=$fails"
[ "$fails" -eq 0 ] && echo "VERDICT=PASS" || echo "VERDICT=FAIL"
exit "$fails"
