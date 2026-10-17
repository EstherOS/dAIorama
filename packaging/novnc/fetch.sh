#!/bin/sh
# Stage the pinned noVNC tree into <dest> (asset root: <data>/novnc).
# Usage: fetch.sh [dest] [tarball]
set -eu
DEST=${1-./novnc}
TARBALL=${2:-}
PIN_SHA256=b1003a11b6e6e8d8f7f5e5586daae7f8ca651d8aee0aa155ff9ac841c48f52c6
URL=https://codeload.github.com/novnc/noVNC/tar.gz/refs/tags/v1.7.0

# Never let a typo'd destination be deleted: "$DEST" is replaced wholesale at the end.
case "$DEST" in
    ""|"/"|"."|"..")
        echo "refusing: destination must be a directory path, got '$DEST'" >&2
        exit 1
        ;;
esac
if [ -e "$DEST" ] && [ ! -f "$DEST/vnc.html" ]; then
    echo "refusing: $DEST exists and is not a noVNC tree (no vnc.html)" >&2
    exit 1
fi

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
if [ -n "$TARBALL" ]; then
    SRC=$TARBALL
else
    SRC=$WORK/noVNC-1.7.0.tar.gz
    curl -fsSLo "$SRC" "$URL"
fi

GOT=$(sha256sum "$SRC" | cut -d' ' -f1)
if [ "$GOT" != "$PIN_SHA256" ]; then
    echo "refusing: noVNC tarball sha256 $GOT != pinned $PIN_SHA256" >&2
    exit 1
fi

mkdir "$WORK/tree"
tar xzf "$SRC" -C "$WORK/tree" --strip-components=1
STAGE="$WORK/stage"
mkdir -p "$STAGE/docs"
cp -R "$WORK/tree/vnc.html" "$WORK/tree/app" "$WORK/tree/core" \
      "$WORK/tree/vendor" "$WORK/tree/LICENSE.txt" "$WORK/tree/AUTHORS" "$STAGE/"
cp "$WORK"/tree/docs/LICENSE.* "$STAGE/docs/"

for f in vnc.html LICENSE.txt docs/LICENSE.MPL-2.0 docs/LICENSE.OFL-1.1 \
         docs/LICENSE.BSD-2-Clause vendor/pako/LICENSE app/styles/base.css core/rfb.js; do
    [ -e "$STAGE/$f" ] || { echo "refusing: staged tree is missing $f" >&2; exit 1; }
done
# The installer creates its own target parent: staging precedes the first run (10), so the
# data root may not exist yet. The binary's own ensure is still the only manager-side creator.
mkdir -p "$(dirname "$DEST")"
rm -rf "$DEST"
mv "$STAGE" "$DEST"
echo "staged noVNC 1.7.0 at $DEST ($(find "$DEST" -type f | wc -l) files)"
