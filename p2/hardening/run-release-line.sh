#!/bin/bash
# C2 release-line live leg: the shipped generator + the two verifiers on one set of freshly
# generated, freshly signed bytes.
#
# Only one keyed binary is built here. The binary embeds `packaging/release/trusted-keys` at
# compile time, so a build that trusts key1 is the shipped source plus that one line; the
# script verifier reads `trusted-keys` from disk beside itself, so the probe gets its
# "provisioned" and "empty" layouts by copying the script, not by rebuilding.
#
# Usage: run-release-line.sh <tag> [source-tarball]
set -u
TAG=${1:-local}
TARBALL=${2:-}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)

KEYS=/tmp/c2linekeys
SRC=/tmp/c2linesrc
rm -rf "$KEYS" "$SRC"
mkdir -p "$KEYS" "$SRC"

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

if [ -n "$TARBALL" ] && [ -f "$TARBALL" ]; then
  tar -xf "$TARBALL" -C "$SRC" || { echo "tar extract failed: $TARBALL"; exit 1; }
  echo "exported tree from $TARBALL: $(tar -tf "$TARBALL" | wc -l) entries"
elif command -v git >/dev/null 2>&1; then
  git -C "$ROOT" archive HEAD | tar -x -C "$SRC" || { echo "git archive failed"; exit 1; }
  echo "exported tree: $(git -C "$ROOT" rev-parse HEAD)"
else
  # No git in the bench: a plain tar of the worktree is the same bytes for this purpose.
  ( cd "$ROOT" && tar -cf - --exclude=./.git --exclude=./target . ) | tar -x -C "$SRC" \
    || { echo "tar export failed"; exit 1; }
  echo "exported tree: tar of $ROOT (no git in this bench)"
fi

cat "$KEYS/key1.pub" >>"$SRC/packaging/release/trusted-keys"
echo "-- trust list as built (tail):"
tail -3 "$SRC/packaging/release/trusted-keys"

( cd "$SRC" && CARGO_TARGET_DIR=/tmp/tk CARGO_TERM_COLOR=never \
    cargo build --locked --bins >"$KEYS/build-key1.log" 2>&1 ) || {
    echo "keyed build failed"; tail -20 "$KEYS/build-key1.log"; exit 1; }
cp /tmp/tk/debug/daiorama /tmp/daiorama-key1 && chmod 755 /tmp/daiorama-key1
echo "keyed binary: $(ls -l /tmp/daiorama-key1 | awk '{print $5}') bytes"
echo "minisign: $(minisign -v 2>&1 | head -1)"

python3 "$HERE/probe-release-line.py" /tmp/daiorama-key1 \
  "$KEYS/key1.pub" "$KEYS/key1.sec" "$KEYS/key2.pub" "$KEYS/key2.sec" \
  "$SRC/packaging/release/verify.sh" "$TAG"
