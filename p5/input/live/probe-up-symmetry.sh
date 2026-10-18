#!/usr/bin/env bash
# Measurement, not a gate: the modifier verbs' refusal symmetry (P5, 05).
#
# `key_down` and `key_up` hold/release modifiers only. A key that could never be a
# modifier is refused by name before any state moves, in both directions; releasing a
# modifier that is not held stays a no-op (release is idempotent). This runs the shipped
# binary's bridge against a real session so the refusals are the handler's, not the
# schema's.
set -u
DAIO=${DAIO:-/out/daiorama-input}; PROBE=${PROBE:-/src/p5/input/live/probe-input-live.py}
export HOME=/root XDG_RUNTIME_DIR=/tmp/xdg XDG_CONFIG_HOME=/root/.config XDG_DATA_HOME=/root/.local/share
export PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin
install -d -m 700 "$XDG_RUNTIME_DIR" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME"
FAILS=0
ok()   { echo "[OK]   $*"; }
fail() { echo "[FAIL] $*"; FAILS=$((FAILS + 1)); }

"$DAIO" config setup >/dev/null 2>&1
SCOPES=$(dirname "$(find "$XDG_CONFIG_HOME" -name operator.ron | head -1)")
cat > "$SCOPES/act.ron" <<'RON'
(
  sessions: (visibility: all),
  limits: (rate: (per_sec: 20, burst: 40, mode: pace, max_wait_ms: 30000, max_queued: 64)),
  audit: (store_clips: false, store_screenshots: false),
  grant: (categories: ["input", "capture", "query"], tools: []),
)
RON
chmod 600 "$SCOPES/act.ron"
"$DAIO" config setup >/dev/null 2>&1
"$DAIO" config set limits.mode off >/dev/null 2>&1
"$DAIO" serve > /tmp/serve.log 2>&1 &
for _ in $(seq 1 60); do "$DAIO" status 2>/dev/null | grep -qi running && break; sleep 1; done
SID=$("$DAIO" new --resolution 1280x720 --scale 1 --label up-symmetry 2>&1 | tail -1 | tr -d '\r')
for _ in $(seq 1 90); do "$DAIO" status "$SID" 2>/dev/null | grep -qi running && break; sleep 1; done
echo "session: $SID"

# call <tool> <args-json> -> probe output to stdout, exit code from the bridge call
call() { python3 "$PROBE" "$DAIO" act "$1" "$2" 2>&1; }

echo
echo "== key_up of a non-modifier is refused by name =="
out=$(call key_up "{\"session_id\": \"$SID\", \"key\": \"x\"}")
echo "$out" | tail -2
case "$out" in
  *bad_request*"releases held modifiers only"*"not a modifier"*) ok "key_up x refused, names the key and the verb" ;;
  *) fail "key_up x did not carry the pinned refusal" ;;
esac

echo
echo "== key_down of a non-modifier is refused by name (the symmetric arm) =="
out=$(call key_down "{\"session_id\": \"$SID\", \"key\": \"x\"}")
echo "$out" | tail -2
case "$out" in
  *bad_request*"holds modifiers only"*"not a modifier"*) ok "key_down x refused, names the key and the verb" ;;
  *) fail "key_down x did not carry the pinned refusal" ;;
esac

echo
echo "== release of an unheld modifier is a no-op, not an error =="
out=$(call key_up "{\"session_id\": \"$SID\", \"key\": \"Shift_L\"}")
echo "$out" | tail -2
case "$out" in
  *"RESULT ok"*) ok "key_up Shift_L on an unheld modifier answers ok" ;;
  *) fail "idempotent release answered an error" ;;
esac

echo
echo "== hold and release a real modifier =="
out=$(call key_down "{\"session_id\": \"$SID\", \"key\": \"Shift_L\"}")
case "$out" in *"RESULT ok"*) ok "key_down Shift_L answers ok" ;; *) fail "key_down Shift_L failed";; esac
out=$(call key_up "{\"session_id\": \"$SID\", \"key\": \"Shift_L\"}")
case "$out" in *"RESULT ok"*) ok "key_up Shift_L answers ok" ;; *) fail "key_up Shift_L failed";; esac

echo
echo "FAILS=$FAILS"
[ "$FAILS" -eq 0 ]
