#!/bin/bash
# C2 install-script live leg: the shipped installer against a real HTTP base and a real keypair.
#
# Two keys are generated; the "provisioned" copy of the shipped script trusts key1 only, so a
# manifest signed by key2 must be refused by name. The shipped script itself (empty trust list)
# is run as-is to prove the trust refusal comes before any fetch.
#
# Usage: run-install-live.sh <tag>
set -u
TAG=${1:-local}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
KEYS=/tmp/instkeys
rm -rf "$KEYS"
mkdir -p "$KEYS"

gen_key() { # gen_key <prefix>
  local prefix="$1" pub="$KEYS/$1.pub" sec="$KEYS/$1.sec"
  if ! minisign -G -W -p "$pub" -s "$sec" >"$KEYS/gen-$prefix.log" 2>&1; then
    printf '\n\n' | minisign -G -p "$pub" -s "$sec" >>"$KEYS/gen-$prefix.log" 2>&1 || {
      echo "minisign keygen failed for $prefix"; cat "$KEYS/gen-$prefix.log"; exit 1; }
  fi
  echo "key $prefix: $(head -1 "$pub")"
}

gen_key key1
gen_key key2
echo "minisign: $(minisign -v 2>&1 | head -1)"
echo "installer: $(cd "$ROOT" && ls -l packaging/release/install.sh | awk '{print $5}') bytes"
echo "tag: $TAG"

python3 "$HERE/probe-install-live.py" "$ROOT/packaging/release/install.sh" \
  "$KEYS/key1.pub" "$KEYS/key1.sec" "$KEYS/key2.pub" "$KEYS/key2.sec"
RC=$?
echo "installlive_rc=$RC"
[ "$RC" -eq 0 ] && echo "VERDICT=PASS" || echo "VERDICT=FAIL"
exit "$RC"
