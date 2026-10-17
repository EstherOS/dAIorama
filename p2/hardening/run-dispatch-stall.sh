#!/bin/bash
# Dispatch-stall battery: does a slow dispatch (create_session waiting on a compositor) stop
# the manager answering anything else?
#
# Setup: limits off (no cgroup), a fake `mutter` on PATH that never becomes ready, so every
# create_session is a dispatch that blocks for the full parent_ready_timeout (15 s).
# Four create_session calls = one per reactor worker. The probe then times a plain `status`.
#
# Usage: run-dispatch-stall.sh <binary> <label>
# Prints the binary by basename only; all roots live under /tmp/dr-dispatch.
set -u
BIN=$(readlink -f "$1")
LABEL=${2:-run}
ROOT=/tmp/dr-dispatch-$LABEL
rm -rf "$ROOT"
mkdir -p "$ROOT"/config/daiorama/default "$ROOT"/data "$ROOT"/run "$ROOT"/bin
printf '(version: 1, limits: (mode: off))\n' > "$ROOT"/config/daiorama/default/config.ron

# A compositor that starts and never reports ready: the readiness wait is the slow dispatch.
cat > "$ROOT"/bin/mutter <<'EOF'
#!/bin/sh
sleep 60
EOF
cat > "$ROOT"/bin/Hyprland <<'EOF'
#!/bin/sh
sleep 60
EOF
chmod +x "$ROOT"/bin/mutter "$ROOT"/bin/Hyprland

export XDG_CONFIG_HOME="$ROOT/config"
export XDG_DATA_HOME="$ROOT/data"
export XDG_RUNTIME_DIR="$ROOT/run"
export TMPDIR="$ROOT"
export PATH="$ROOT/bin:$PATH"

"$BIN" serve > "$ROOT/serve.log" 2>&1 &
SERVE=$!
for _ in $(seq 1 100); do
    [ -S "$ROOT/run/daiorama/default/manager.sock" ] && break
    sleep 0.1
done

python3 "$(dirname "$0")/probe-dispatch-stall.py" \
    "$ROOT/run/daiorama/default/manager.sock" \
    "$(basename "$BIN")" "$LABEL"

RC=$?
kill $SERVE 2>/dev/null
wait $SERVE 2>/dev/null
exit $RC
