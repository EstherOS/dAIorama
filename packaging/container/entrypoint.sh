#!/bin/sh
# Seed the pinned noVNC tree into this instance's data root, then run the command.
#
# The tree is data, not an image layer (07/09): the desktop image carries one read-only copy
# at $DAIORAMA_NOVNC_SEED and this script copies it onto the persisted volume only when the
# front's entry point is absent. A recreate keeps the volume's tree; an operator can restage
# or upgrade it with packaging/novnc/fetch.sh.
#
# It creates no other directory: 08 single-sources directory creation in the binary, so the
# runtime dir is the manager's job, not the entrypoint's.
set -eu

if [ "$#" -eq 0 ]; then
    echo "usage: entrypoint.sh <command> [args...]" >&2
    exit 2
fi

SEED="${DAIORAMA_NOVNC_SEED:-/usr/share/daiorama/novnc}"
DATA_HOME="${XDG_DATA_HOME:-/data}"
INSTANCE="${DAIORAMA_INSTANCE:-default}"
DEST="$DATA_HOME/daiorama/$INSTANCE/novnc"

if [ ! -f "$SEED/vnc.html" ]; then
    echo "daiorama: no seeded view assets at $SEED; views refuse view_unavailable until the tree is staged" >&2
elif [ -f "$DEST/vnc.html" ]; then
    :
else
    mkdir -p "$DEST"
    cp -a "$SEED/." "$DEST/"
    echo "daiorama: staged view assets at $DEST"
fi

exec "$@"
