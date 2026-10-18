#!/usr/bin/env python3
"""Rate-limiter bucket-map growth leg against a real `daiorama serve`.

`dispatch()` spends a control-class token *before* any session lookup and keys the bucket by
the caller-supplied `session_id`, so a call that names nothing still leaves an entry in the
manager's `RateLimiter` map. This leg measures what those entries cost: the manager's VmRSS
across *repeated* identical loads. Allocator retention reaches a steady state after the first
repetition; a map that keeps every key keeps growing. So the assertion is flatness of the
second and third repetitions, not an absolute delta.

Every id used here is one no session can have (longer than the store's 128-byte bound), so
every answer is `session_not_found`: the frame is accepted, the gate runs, nothing else does.

Cases:
  [A] baseline VmRSS after a handshake.
  [B] three rounds of 16 control calls with unique 1 MiB ids -- the last two rounds must be
      flat. An unbounded map adds ~1.5 MiB per call, every round.
  [C] two control calls carrying a 6 MiB id; the second must be flat (single-call amplifier).
  [D] a real session id still paces: burst 2 reject, the third call `rate_limited`.
  [E] the manager is still healthy afterwards (`status` answers).

Usage: probe-rate-buckets.py <manager.sock> <manager-pid> <operator.ron path>
Prints [OK]/[FAIL]/[OBS] and FAILS=<n>. Exit code = FAILS (capped at 120).
"""
import json
import socket
import sys
import time

SOCK = sys.argv[1]
PID = int(sys.argv[2])
SCOPE_FILE = sys.argv[3]
SCOPE = "operator"
PROTO = "0.1.0"

# Steady-state noise bound: a repeated round reuses the same buffers, so a bounded map moves
# RSS by allocator noise only. The unbounded map moves it by ~1.5 MiB per 1 MiB id.
FLAT_KB = 2048

fails = 0


def ok(label, detail=""):
    print(f"[OK]   {label}" + (f" ({detail})" if detail else ""))


def fail(label, detail):
    global fails
    fails += 1
    print(f"[FAIL] {label} :: {detail}")


def check(label, cond, detail):
    if cond:
        ok(label, detail)
    else:
        fail(label, detail)


def obs(label):
    print(f"[OBS]  {label}")


def rss_kb(pid):
    with open(f"/proc/{pid}/status") as fh:
        for line in fh:
            if line.startswith("VmRSS:"):
                return int(line.split()[1])
    raise RuntimeError(f"no VmRSS for {pid}")


def settle_rss(pid, pause=0.6):
    time.sleep(pause)
    return rss_kb(pid)


def write_scope(text):
    with open(SCOPE_FILE, "w") as fh:
        fh.write(text)
    time.sleep(0.02)


def reject_scope(per_sec=1, burst=2, max_wait_ms=30000, max_queued=64):
    write_scope(
        "(\n"
        "  sessions: (visibility: all),\n"
        f"  limits: (rate: (per_sec: {per_sec}, burst: {burst}, mode: reject, "
        f"max_wait_ms: {max_wait_ms}, max_queued: {max_queued})),\n"
        ")\n"
    )


def connect():
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(60)
    s.connect(SOCK)
    f = s.makefile("rwb")
    f.write(
        (
            json.dumps(
                {
                    "method": "handshake",
                    "params": {"protocol_version": PROTO, "min_supported": PROTO},
                }
            )
            + "\n"
        ).encode()
    )
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


def error_code(reply):
    return (reply.get("error") or {}).get("code")


def big_id(tag, size):
    """A unique id of `size` bytes: longer than the 128-byte session-id bound, so the store
    refuses it and the registry misses it -- but the raw string reaches the rate gate."""
    return f"{tag}-" + "x" * (size - len(tag) - 1)


MIB = 1024 * 1024

s, f = connect()
try:
    # -----------------------------------------------------------------------
    # [A] baseline (one call first so the parse path is warm)
    # -----------------------------------------------------------------------
    control(f, "warmup")
    baseline = settle_rss(PID)
    ok("[A1] manager is running", f"pid={PID}")
    obs(f"[A2] baseline VmRSS = {baseline} kB")

    # -----------------------------------------------------------------------
    # [B] three rounds of 16 unique 1 MiB ids
    # -----------------------------------------------------------------------
    round_deltas = []
    first_answer = None
    previous = baseline
    for round_index in range(3):
        started = time.monotonic()
        for call_index in range(16):
            answer = control(f, big_id(f"r{round_index}c{call_index}", MIB))
            if first_answer is None:
                first_answer = answer
        elapsed = time.monotonic() - started
        current = settle_rss(PID)
        round_deltas.append(current - previous)
        obs(
            f"[B{round_index + 1}] round {round_index + 1}: 16 x 1 MiB ids in {elapsed:.2f}s; "
            f"VmRSS {previous} -> {current} kB (delta {current - previous} kB)"
        )
        previous = current

    check(
        "[B4] every id answered `session_not_found` (the frame passed the gate)",
        error_code(first_answer) == "session_not_found",
        json.dumps(first_answer)[:200],
    )
    check(
        "[B5] a second round of fresh ids does not grow the manager",
        round_deltas[1] < FLAT_KB,
        f"round2 delta={round_deltas[1]} kB (bound {FLAT_KB} kB)",
    )
    check(
        "[B6] a third round stays flat (steady state, not a slower leak)",
        round_deltas[2] < FLAT_KB,
        f"round3 delta={round_deltas[2]} kB (bound {FLAT_KB} kB)",
    )

    # -----------------------------------------------------------------------
    # [C] repeated 6 MiB id -- the single-call amplifier. The first calls pay the allocator's
    # one-time arena growth for frames that size (glibc keeps a few arenas, it does not return
    # them); a bounded map plateaus there, while an unbounded one keeps another ~6 MiB key per
    # call and cannot fit under the ceiling.
    # -----------------------------------------------------------------------
    huge_deltas = []
    previous = rss_kb(PID)
    huge_answers = []
    for index in range(12):
        huge_answers.append(control(f, big_id(f"huge{index}", 6 * MIB)))
        current = settle_rss(PID)
        huge_deltas.append(current - previous)
        obs(f"[C1] 6 MiB id #{index + 1}: VmRSS {previous} -> {current} kB (delta {current - previous} kB)")
        previous = current
    huge_total = sum(huge_deltas)
    check(
        "[C2] the 6 MiB ids are refused as unknown sessions",
        all(error_code(answer) == "session_not_found" for answer in huge_answers),
        " / ".join(error_code(answer) or "none" for answer in huge_answers),
    )
    check(
        "[C3] twelve 6 MiB ids plateau instead of accumulating",
        huge_total < 80 * 1024,
        f"total={huge_total} kB over 12 calls (ceiling {80 * 1024} kB; an unbounded map adds ~6 MiB/call)",
    )
finally:
    s.close()

# ---------------------------------------------------------------------------
# [D] pacing still works for a real id
# ---------------------------------------------------------------------------
reject_scope()
s, f = connect()
try:
    d1 = control(f, "sess-pace")
    d2 = control(f, "sess-pace")
    d3 = control(f, "sess-pace")
    check(
        "[D1] the first two calls pass the bucket",
        error_code(d1) == "session_not_found" and error_code(d2) == "session_not_found",
        f"{error_code(d1)} / {error_code(d2)}",
    )
    check("[D2] the third call is rate_limited", error_code(d3) == "rate_limited", json.dumps(d3)[:200])

    # -----------------------------------------------------------------------
    # [E] the manager is still healthy
    # -----------------------------------------------------------------------
    e1 = call(f, "status", {})
    check("[E1] status still answers after the id burst", "result" in e1, json.dumps(e1)[:200])
finally:
    s.close()

print(f"FAILS={fails}")
sys.exit(min(fails, 120))
