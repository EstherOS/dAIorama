#!/bin/sh
# Both-ways evidence for the verifier's read bounds (09 C2).
#   PRE  = merged ae7c6ac binary (pre-fix)
#   POST = p5/release-hardening-esther binary
# Both binaries in this run were built with the fixture TEST public key appended to
# packaging/release/trusted-keys (source reverted afterwards), so the embedded trust-list path
# is exercised end to end. Override binaries with PRE=... POST=... .
set -eu
PRE=${PRE:-/home/agent/.hermes/cache/scratch/target-prefix/debug/daiorama}
POST=${POST:-/home/agent/.hermes/cache/scratch/target-hardening/debug/daiorama}
HERE=$(cd "$(dirname "$0")" && pwd)
FIX=$(cd "$HERE/../../../tests/fixtures/release" && pwd)
MEASURE="python3 $HERE/measure-rss.py"
WORK=$(mktemp -d)
cd "$WORK"
cp "$FIX/manifest.json" fixture-manifest.json
cp "$FIX/manifest.json.minisig" fixture-manifest.json.minisig
NAME=daiorama-0.1.0-x86_64-unknown-linux-gnu
truncate -s 135266304 "$NAME"          # 129 MiB sparse: over the 128 MiB artifact cap
truncate -s 2097152 big.json           # 2 MiB: over the 1 MiB document cap

for side in pre post; do
    bin=$PRE
    label="pre-fix (merged ae7c6ac)"
    if [ "$side" = post ]; then
        bin=$POST
        label="post-fix (p5/release-hardening-esther)"
    fi
    echo "===== $label"
    echo "-- signed fixture (trust-list path, no artifact):"
    "$bin" release verify --manifest fixture-manifest.json \
        --signature fixture-manifest.json.minisig || true
    echo "-- 129 MiB artifact:"
    $MEASURE "$bin" release verify --manifest fixture-manifest.json \
        --signature fixture-manifest.json.minisig --artifact "$NAME" || true
    echo "-- 2 MiB manifest named as a manifest:"
    $MEASURE "$bin" release verify --manifest big.json \
        --signature fixture-manifest.json.minisig || true
    echo
done
