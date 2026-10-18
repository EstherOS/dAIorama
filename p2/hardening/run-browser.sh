#!/bin/bash
# D6 live leg: `daiorama-browser` on a box with no compositor -- the failure and capture
# semantics the desktop image relies on, driven with a fake browser and a fake `hyprctl`.
#
# Usage: probe-browser.sh <daiorama-browser> <label>
set -u
LAUNCHER=$(readlink -f "$1")
LABEL=${2:-run}
ROOT=/tmp/db-$LABEL
rm -rf "$ROOT"
mkdir -p "$ROOT"/run "$ROOT"/bin "$ROOT"/data "$ROOT"/nohypr "$ROOT"/plain
fails=0
ok()  { printf '[OK]   %s%s\n' "$1" "${2:+ ($2)}"; }
bad() { fails=$((fails + 1)); printf '[FAIL] %s :: %s\n' "$1" "$2"; }
chk() { if [ "$2" = "$3" ]; then ok "$1" "$3"; else bad "$1" "got '$2', want '$3'"; fi; }

# A browser that does nothing for a moment and then exits 3.
cat > "$ROOT"/bin/fake-browser <<'EOF'
#!/bin/sh
sleep 0.4
exit 3
EOF
chmod 755 "$ROOT"/bin/fake-browser
cp "$ROOT"/bin/fake-browser "$ROOT"/plain/fake-browser

# A compositor list that reports the probe title the way Hyprland does.
cat > "$ROOT"/bin/hyprctl <<'EOF'
#!/bin/sh
printf '[{"address":"0x1","title":"daiorama-renderer: llvmpipe (LLVM 17.0.6, 256 bits)"}]\n'
EOF
chmod 755 "$ROOT"/bin/hyprctl

# A compositor list that never carries the probe title.
cat > "$ROOT"/nohypr/hyprctl <<'EOF'
#!/bin/sh
printf '[{"address":"0x1","title":"something else"}]\n'
EOF
chmod 755 "$ROOT"/nohypr/hyprctl

RUN() { XDG_RUNTIME_DIR="$ROOT/run" XDG_DATA_HOME="$ROOT/data" "$@" 2>&1; }

echo "== [A] no runtime dir =="
OUT=$(env -u XDG_RUNTIME_DIR "$LAUNCHER" --browser "$ROOT/bin/fake-browser" http://example.test 2>&1)
RC=$?
chk "a launcher with no XDG_RUNTIME_DIR exits 127" "$RC" "127"
case "$OUT" in *"XDG_RUNTIME_DIR is not set"*) ok "the refusal names XDG_RUNTIME_DIR";; *) bad "the refusal names XDG_RUNTIME_DIR" "$OUT";; esac

echo "== [B] no compositor IPC =="
OUT=$(PATH="$ROOT/plain:/usr/bin:/bin" XDG_RUNTIME_DIR="$ROOT/run" XDG_DATA_HOME="$ROOT/data" \
    "$LAUNCHER" --browser "$ROOT/plain/fake-browser" --capture-timeout 1 http://example.test 2>&1)
RC=$?
chk "no hyprctl exits 127" "$RC" "127"
case "$OUT" in *"hyprctl is not available"*) ok "the refusal names the missing compositor IPC";; *) bad "the refusal names the missing compositor IPC" "$OUT";; esac
[ -e "$ROOT/run/browser-renderer" ] && bad "no renderer file is written" "file exists" || ok "no renderer file is written"
[ -e "$ROOT/run/browser-probe.html" ] && bad "the probe page is cleaned up" "probe still present" || ok "the probe page is cleaned up"

echo "== [C] a title on the compositor list is captured =="
OUT=$(PATH="$ROOT/bin:/usr/bin:/bin" XDG_RUNTIME_DIR="$ROOT/run" XDG_DATA_HOME="$ROOT/data" \
    "$LAUNCHER" --browser "$ROOT/bin/fake-browser" --capture-timeout 5 http://example.test 2>&1)
RC=$?
chk "the launcher keeps the browser's exit status" "$RC" "3"
chk "the renderer line is the title suffix" "$(cat "$ROOT/run/browser-renderer" 2>/dev/null)" \
    "renderer=llvmpipe (LLVM 17.0.6, 256 bits)"
chk "the renderer file is 0600" "$(stat -c '%a' "$ROOT/run/browser-renderer" 2>/dev/null)" "600"
[ -e "$ROOT/run/browser-probe.html" ] && bad "the probe page is cleaned up after capture" "probe still present" || ok "the probe page is cleaned up after capture"
chk "no temp renderer file is left behind" "$(ls -A "$ROOT/run" | grep -c '^\.browser-renderer\.tmp')" "0"

echo "== [D] a timeout is not fatal =="
rm -f "$ROOT/run/browser-renderer"
OUT=$(PATH="$ROOT/nohypr:/usr/bin:/bin" XDG_RUNTIME_DIR="$ROOT/run" XDG_DATA_HOME="$ROOT/data" \
    "$LAUNCHER" --browser "$ROOT/bin/fake-browser" --capture-timeout 1 http://example.test 2>&1)
RC=$?
chk "a capture timeout keeps the browser's exit status" "$RC" "3"
case "$OUT" in *"no renderer title within 1s"*) ok "the timeout warns once";; *) bad "the timeout warns once" "$OUT";; esac
[ -e "$ROOT/run/browser-renderer" ] && bad "no renderer file after a timeout" "file exists" || ok "no renderer file after a timeout"
[ -e "$ROOT/run/browser-probe.html" ] && bad "the probe page is cleaned up after a timeout" "probe still present" || ok "the probe page is cleaned up after a timeout"

echo "FAILS=$fails"
[ "$fails" -eq 0 ] && echo "VERDICT=PASS" || echo "VERDICT=FAIL"
exit "$fails"
