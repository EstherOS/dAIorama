#!/usr/bin/env python3
"""History-state leg against a real `daiorama serve`: invented ids leave no trace.

`dispatch()` records every control call and `History::append` keys a per-session cache and
creates `<data>/history/<id>`. Before this slice, a valid id naming no session still wrote a
directory and a map entry; a caller choosing ids could grow both without bound. This leg sends
control calls for ids that are valid slugs but name nothing, then asserts:

  [A] every answer is `session_not_found`;
  [B] the history root has no session directory after the burst (no record, no dir);
  [C] three rounds of the same load keep VmRSS flat (the cache is bounded);
  [D] the manager still answers `status` afterwards.

Usage: probe-history-bound.py <manager.sock> <manager-pid> <history-root>
Prints [OK]/[FAIL]/[OBS]; exit code = FAILS.
"""
import json
import os
import socket
import sys
import time

SOCK, PID, HISTORY = sys.argv[1], int(sys.argv[2]), sys.argv[3]
SCOPE = "operator"
PROTO = "0.1.0"
ROUNDS, PER_ROUND = 3, 64
FLAT_KB = 2048

fails = 0


def check(label, cond, detail):
    global fails
    if cond:
        print(f"[OK]   {label} ({detail})")
    else:
        fails += 1
        print(f"[FAIL] {label} :: {detail}")


def rss_kb():
    with open(f"/proc/{PID}/status") as fh:
        for line in fh:
            if line.startswith("VmRSS:"):
                return int(line.split()[1])
    raise RuntimeError("no VmRSS")


def connect():
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(60)
    s.connect(SOCK)
    f = s.makefile("rwb")
    f.write((json.dumps({"method": "handshake", "params": {
        "protocol_version": PROTO, "min_supported": PROTO}}) + "\n").encode())
    f.flush()
    reply = json.loads(f.readline())
    if "result" not in reply:
        raise RuntimeError(f"handshake refused: {reply}")
    return s, f


def call(f, method, params):
    f.write((json.dumps({"method": method, "params": params, "scope": SCOPE}) + "\n").encode())
    f.flush()
    return json.loads(f.readline())


def control(f, session_id):
    return call(f, "pointer_move", {"session_id": session_id, "x": 1, "y": 1})


def session_dirs():
    try:
        return sorted(os.listdir(HISTORY))
    except FileNotFoundError:
        return []


s, f = connect()
try:
    # [A] the burst itself
    wrong = []
    for round_index in range(ROUNDS):
        for index in range(PER_ROUND):
            sid = f"invented-{round_index}-{index}"
            reply = control(f, sid)
            code = (reply.get("error") or {}).get("code")
            if code != "session_not_found":
                wrong.append((sid, reply))
        time.sleep(0.2)
        if round_index == 0:
            first = rss_kb()
        elif round_index == ROUNDS - 1:
            last = rss_kb()
    check("[A] every invented id is session_not_found", not wrong,
          f"{ROUNDS * PER_ROUND} calls" + (f", first bad: {wrong[0]}" if wrong else ""))
    # [B] no history state was created
    dirs = session_dirs()
    check("[B] history root holds no session directory", dirs == [], f"{HISTORY}: {dirs[:3]}")
    # [C] bounded cache: the repeated load is flat
    check("[C] VmRSS is flat across rounds", abs(last - first) <= FLAT_KB,
          f"{first} kB -> {last} kB")
    # [D] still healthy
    status = call(f, "status", {})
    check("[D] manager still answers status", "result" in status, str(status)[:120])
finally:
    s.close()

print(f"FAILS={fails}")
sys.exit(min(fails, 120))
