#!/bin/sh
# Both-ways evidence for the generator's pinned byte layout (09 C2).
#   PRE  = merged ae7c6ac binary (pre-fix)
#   POST = p5/release-hardening-esther binary
# Override with PRE=... POST=... . Writes into a fresh temp dir; printed paths are relative.
set -eu
PRE=${PRE:-/home/agent/.hermes/cache/scratch/target-prefix/debug/daiorama}
POST=${POST:-/home/agent/.hermes/cache/scratch/target-hardening/debug/daiorama}
WORK=$(mktemp -d)
cd "$WORK"
NAME=daiorama-0.1.0-x86_64-unknown-linux-gnu
head -c 1024 /dev/zero > "$NAME"

for side in pre post; do
    bin=$PRE
    label="pre-fix (merged ae7c6ac)"
    if [ "$side" = post ]; then
        bin=$POST
        label="post-fix (p5/release-hardening-esther)"
    fi
    "$bin" release manifest \
        --version 0.1.0 --protocol 0.1.0 --released-at 2026-10-18T00:00:00Z \
        --artifact "core:x86_64-unknown-linux-gnu:$PWD/$NAME" \
        --out "$side.json" >/dev/null
    echo "== $label"
    echo "-- pinned grep -F count (expect 1):"
    grep -F "\"name\":\"$NAME\"" "$side.json" | wc -l
    echo "-- pinned sed digest (expect 64 hex):"
    sed -n 's/.*"sha256":"\([0-9a-f]\{64\}\)".*/\1/p' "$side.json"
    echo "-- artifact line:"
    grep -F '"name":' "$side.json" | head -1
    echo
done
