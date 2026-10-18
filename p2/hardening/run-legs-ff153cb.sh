#!/bin/bash
# Merged-tip battery at ff153cb: every carried live leg, including the three that arrived after
# the last full pass at dc59f6e -- rate-buckets (#117 bucket bounds), history-bound (#118
# invented-id hygiene) and install-live (C2 installer). Disposable Debian-based container:
# python3 + minisign installed, binaries built cold.
set -u
cd /src || exit 1
export CARGO_TERM_COLOR=never
export DEBIAN_FRONTEND=noninteractive
logdir=/src/p2/hardening/evidence

apt-get update -qq >/tmp/apt.log 2>&1
apt-get install -y -qq python3 minisign curl >>/tmp/apt.log 2>&1 || {
  echo "apt install failed"; tail -8 /tmp/apt.log; exit 1; }
echo "python3 $(python3 --version 2>&1)"
echo "minisign $(minisign -v 2>&1 | head -1)"

export CARGO_TARGET_DIR=/tmp/t
cargo build --workspace --bins --locked >/tmp/build.log 2>&1 || {
  echo "build failed"; tail -25 /tmp/build.log; exit 1; }
BIN=/tmp/t/debug/daiorama
MOD=/tmp/t/debug/daiorama-dashboard
LAUNCH=/tmp/t/debug/daiorama-browser
echo "binaries: $(ls -l "$BIN" "$MOD" "$LAUNCH" | awk '{print $5, $9}')"

fails=0
runleg() { # runleg <name> <cmd...>
  local name=$1; shift
  "$@" >"$logdir/$name-ff153cb.log" 2>&1
  local rc=$?
  echo "$name rc=$rc"
  [ "$rc" -ne 0 ] && fails=$((fails + 1))
  return 0
}

runleg dispatch-stall bash p2/hardening/run-dispatch-stall.sh "$BIN" ff153cb
runleg rate-limit bash p2/hardening/run-rate-limit.sh "$BIN" ff153cb
runleg rate-buckets bash p2/hardening/run-rate-buckets.sh "$BIN" ff153cb
runleg history bash p2/hardening/run-history.sh "$BIN" ff153cb
runleg history-bound bash p2/hardening/run-history-bound.sh "$BIN" ff153cb
runleg dashboard-module bash p2/hardening/run-dashboard-module.sh "$BIN" ff153cb
runleg scope-create bash p2/hardening/run-scope-create.sh "$BIN" ff153cb
runleg dashboard-install bash p2/hardening/run-dashboard-install.sh "$BIN" "$MOD" ff153cb
runleg browser bash p2/hardening/run-browser.sh "$LAUNCH" ff153cb
runleg install-rows bash p2/hardening/run-install-rows.sh "$BIN"
runleg release-line bash p2/hardening/run-release-line.sh ff153cb
runleg install-live bash p2/hardening/run-install-live.sh ff153cb

# capture suite under parallel load (3 concurrent cold runs, own target dir)
export CARGO_TARGET_DIR=/tmp/tc
for index in 1 2 3; do
  ( cargo test --locked capture >"$logdir/capture-parallel-$index-ff153cb.log" 2>&1;
    echo "cap$index=$?" >"$logdir/capture-parallel-$index.rc" ) &
done
wait
for index in 1 2 3; do cat "$logdir/capture-parallel-$index.rc"; done
export CARGO_TARGET_DIR=/tmp/t

echo "LEG_FAILS=$fails"
exit "$fails"
