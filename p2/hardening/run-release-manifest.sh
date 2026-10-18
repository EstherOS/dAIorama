#!/bin/bash
# C2 manifest live leg: a real minisign keypair against the shipped generator and verifier.
#
# The trust list is `include_str!`-embedded, so "a build that trusts key X" is a real build.
# This script generates two throwaway keys, produces three binaries from the same source --
# empty trust list (the shipped state), key1 appended, key1+key2 appended (the rotation
# window) -- and hands them to probe-release-manifest.py.
#
# The source is exported with `git archive HEAD`, so the container never dirties the
# worktree and the keyed builds are exactly the committed tree plus one trust-list line.
#
# Usage: run-release-manifest.sh <daiorama-binary> <tag> [source-tarball]
set -u
BIN=$(readlink -f "$1")
TAG=${2:-local}
TARBALL=${3:-}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)

KEYS=/tmp/c2keys
SRC=/tmp/c2src
rm -rf "$KEYS" "$SRC"
mkdir -p "$KEYS" "$SRC"

gen_key() { # gen_key <prefix>
  local prefix="$1" pub="$KEYS/$1.pub" sec="$KEYS/$1.sec"
  if ! minisign -G -W -p "$pub" -s "$sec" >/tmp/c2keys/gen-$prefix.log 2>&1; then
    printf '\n\n' | minisign -G -p "$pub" -s "$sec" >>/tmp/c2keys/gen-$prefix.log 2>&1 || {
      echo "minisign keygen failed for $prefix"; cat /tmp/c2keys/gen-$prefix.log; exit 1; }
  fi
  echo "key $prefix: $(head -1 "$pub")"
}

gen_key key1
gen_key key2

if [ -n "$TARBALL" ] && [ -f "$TARBALL" ]; then
  tar -xf "$TARBALL" -C "$SRC" || { echo "tar extract failed: $TARBALL"; exit 1; }
  echo "exported tree from $TARBALL: $(tar -tf "$TARBALL" | wc -l) entries"
else
  git -C "$ROOT" archive HEAD | tar -x -C "$SRC" || { echo "git archive failed"; exit 1; }
  echo "exported tree: $(git -C "$ROOT" rev-parse HEAD)"
fi

build_keyed() { # build_keyed <out-name>
  ( cd "$SRC" && CARGO_TARGET_DIR=/tmp/tk CARGO_TERM_COLOR=never \
      cargo build --locked --bins >/tmp/c2keys/build-$1.log 2>&1 ) || {
      echo "keyed build failed ($1)"; tail -20 /tmp/c2keys/build-$1.log; exit 1; }
  cp /tmp/tk/debug/daiorama "/tmp/$1" && chmod 755 "/tmp/$1"
  echo "keyed binary $1: $(ls -l "/tmp/$1" | awk '{print $5}') bytes"
}

cat "$KEYS/key1.pub" >> "$SRC/packaging/release/trusted-keys"
build_keyed daiorama-key1
cat "$KEYS/key2.pub" >> "$SRC/packaging/release/trusted-keys"
build_keyed daiorama-key12

echo "-- trust list as built (tail):"
tail -6 "$SRC/packaging/release/trusted-keys"

python3 "$HERE/probe-release-manifest.py" "$BIN" /tmp/daiorama-key1 /tmp/daiorama-key12 \
  "$TAG" "$KEYS/key1.pub" "$KEYS/key1.sec" "$KEYS/key2.pub" "$KEYS/key2.sec"
