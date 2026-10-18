#!/usr/bin/env bash
# Changed-surface pass at the merged tip 09c6683: the input line, end to end.
#
# The three commits between f69e3c9 (last full battery) and 09c6683 all touch the
# act half -- the keymap settle / modifiers / one-character keysyms (#129), the
# manager-side modifier state for cross-call chords (#130), and key_up's refusal
# symmetry (#131). The rest of the tree is unchanged, so this pass covers the gates
# plus the one leg that owns that surface, on the merged tip rather than on a branch.
#
# Phases (a fresh dbus session each, as each script expects):
#   1. run-input-live.sh     21 assertions: pointer via hyprctl cursorpos, keyboard
#                            bytes through a raw tty, the scale-2 pixel conversion
#   2. probe-chord.sh        cross-call chord: CHORD=works, SHIFTED_TAP=works
#   3. probe-up-symmetry.sh  key_down/key_up refuse a non-modifier by name; an unheld
#                            release stays a no-op
#
# Runs in the L1 image (privileged, /dev/dri) with the binary built from this tip at
# /out/daiorama-input-09c6683 and the tree mounted read-only at /src.
set -u
cd /src || exit 1
DAIO=${DAIO:-/out/daiorama-input-09c6683}
export DAIO
fails=0
phase() { # phase <label> <script>
  echo
  echo "################################################################"
  echo "## $1"
  echo "################################################################"
  dbus-run-session -- bash "$2"
  local rc=$?
  echo "PHASE_RC=$rc"
  [ "$rc" -ne 0 ] && fails=$((fails + 1))
  return 0
}

phase "[1] input live leg (pointer, keyboard bytes, scale 2)" p5/input/live/run-input-live.sh
phase "[2] cross-call chord (P5-INPUT-CHORD, #130)" p5/input/live/probe-chord.sh
phase "[3] modifier refusal symmetry (P5-INPUT-UP-SYMMETRY, #131)" p5/input/live/probe-up-symmetry.sh

echo
echo "SURFACE_FAILS=$fails"
exit "$fails"
