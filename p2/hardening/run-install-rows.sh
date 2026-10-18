#!/bin/bash
# C2 install-row live leg: one real binary, `/etc/os-release` rewritten per release, the
# operator-visible `doctor` lines asserted against the maintained table
# (`packaging/install-commands.md`).
#
# The probe restores /etc/os-release on exit; run it in a disposable container.
#
# Usage: run-install-rows.sh <binary>
set -u
BIN=$(readlink -f "$1")
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
exec python3 "$HERE/probe-install-rows.py" "$BIN" "$ROOT"
