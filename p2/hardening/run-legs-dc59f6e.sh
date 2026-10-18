#!/bin/bash
# Merged-tip battery at dc59f6e: every carried live leg plus the new release-line leg.
# Disposable Debian-based container: python3 + minisign installed, binaries built cold.
set -u
cd /src || exit 1
export CARGO_TERM_COLOR=never
export DEBIAN_FRONTEND=noninteractive
logdir=/src/p2/hardening/evidence

apt-get update -qq >/tmp/apt.log 2>&1
apt-get install -y -qq python3 minisign >>/tmp/apt.log 2>&1 || {
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
  "$@" >"$logdir/$name-dc59f6e.log" 2>&1
  local rc=$?
  echo "$name rc=$rc"
  [ "$rc" -ne 0 ] && fails=$((fails + 1))
}

runleg dispatch-stall bash p2/hardening/run-dispatch-stall.sh "$BIN" dc59f6e
runleg rate-limit bash p2/hardening/run-rate-limit.sh "$BIN" dc59f6e
runleg history bash p2/hardening/run-history.sh "$BIN" dc59f6e
runleg dashboard-module bash p2/hardening/run-dashboard-module.sh "$BIN" dc59f6e
runleg scope-create bash p2/hardening/run-scope-create.sh "$BIN" dc59f6e
runleg dashboard-install bash p2/hardening/run-dashboard-install.sh "$BIN" "$MOD" dc59f6e
runleg browser bash p2/hardening/run-browser.sh "$LAUNCH" dc59f6e
runleg install-rows bash p2/hardening/run-install-rows.sh "$BIN"

# capture suite under parallel load (3 concurrent cold runs, own target dir)
export CARGO_TARGET_DIR=/tmp/tc
for index in 1 2 3; do
  ( cargo test --locked capture >"$logdir/capture-parallel-$index-dc59f6e.log" 2>&1;
    echo "cap$index=$?" >"$logdir/capture-parallel-$index.rc" ) &
done
wait
for index in 1 2 3; do cat "$logdir/capture-parallel-$index.rc"; done
export CARGO_TARGET_DIR=/tmp/t

runleg release-line bash p2/hardening/run-release-line.sh dc59f6e

echo "LEG_FAILS=$fails"
exit "$fails"
