#!/bin/sh
# Merged-tip gates at 09c6683: fmt, clippy, build, workspace tests. Cold target dir.
set -u
export CARGO_TARGET_DIR=/tmp/t
export CARGO_TERM_COLOR=never
log=/src/p2/hardening/evidence/gate-09c6683${GATE_LABEL:+-$GATE_LABEL}.log
: >"$log"
say() { printf '%s\n' "$*" | tee -a "$log"; }
run() { # run <label> <cmd...>
  label=$1; shift
  say "== $label"
  "$@" >>"$log" 2>&1
  rc=$?
  say "rc=$rc"
  return $rc
}
fails=0
rustup component add rustfmt clippy >>"$log" 2>&1 || say "rustup component add rc=$?"
say "rustc $(rustc --version)"
say "cargo $(cargo --version)"
run fmt cargo fmt --all || fails=$((fails+1))
run fmt-check cargo fmt --all --check || fails=$((fails+1))
run clippy cargo clippy --workspace --all-targets --locked -- -D warnings || fails=$((fails+1))
run build cargo build --workspace --bins --locked || fails=$((fails+1))
run test cargo test --workspace --locked || fails=$((fails+1))
say "GATE_FAILS=$fails"
exit "$fails"
