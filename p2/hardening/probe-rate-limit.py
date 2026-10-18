#!/usr/bin/env python3
"""B9 rate-limit live leg against a real `daiorama serve` and its real unix socket.

The unit tests in `src/runtime/rate.rs` exercise the bucket in isolation. This probe
exercises the shipped path instead: a real manager process, real frames, the scope file
on disk read per call (`scope_definition()`), and the check ordering in `dispatch()`
(control-class calls spend a token before anything else).

Every session id here is a *closed* session seeded by `known_session`: since #117/#118 the
gate spends a token only for an id that can name a session, and this bench has no compositor
to create a live one. The handler still answers `session_not_found` (asserted below), so what
the leg measures is the bucket in front of it.

Cases:
  [A] reject mode, per_sec 1, burst 2 -- two control calls pass, the third answers
      `rate_limited` with a numeric `retry after <N> ms` hint.
  [B] while that bucket is empty, `session_status` and `screenshot` on the same session
      are NOT rate limited (reads and capture are exempt).
  [C] a second session id still has its own burst (bucket is per (scope, session)).
  [D] after 1.2 s the first session passes again (continuous refill).
  [E] the scope file is re-read per call: rewriting it to pace/per_sec 2 with no manager
      restart makes the next over-burst call wait ~0.5 s instead of refusing.
  [F] pace mode with max_wait_ms 20 refuses rather than parks a long wait.
  [G] 6 concurrent control calls, burst 1, max_queued 2 -- the flood degrades to
      refusals instead of parking everyone, and answers inside the parked wait bound.

Usage: probe-rate-limit.py <manager.sock> <operator.ron path> <instance data dir>
Prints [OK]/[FAIL] per assertion, a note per observation, and FAILS=<n>.
Exit code = FAILS (capped at 120).
"""
import json
import os
import socket
import sys
import threading
import time

import known_session

SOCK = sys.argv[1]
SCOPE_FILE = sys.argv[2]
DATA = sys.argv[3]
SCOPE = "operator"
PROTO = "0.1.0"

fails = 0


def ok(label, detail=""):
    print(f"[OK]   {label}" + (f" ({detail})" if detail else ""))


def fail(label, detail):
    global fails
    fails += 1
    print(f"[FAIL] {label} :: {detail}")


def note(label):
    print(f"[NOTE] {label}")


def check(label, cond, detail):
    if cond:
        ok(label, detail)
    else:
        fail(label, detail)


def write_scope(text):
    with open(SCOPE_FILE, "w") as fh:
        fh.write(text)
    # The manager reads the file per call; make sure the write is visible before the
    # next frame (same filesystem, same process namespace).
    time.sleep(0.02)


def reject_scope(per_sec=1, burst=2, max_wait_ms=30000, max_queued=64):
    write_scope(
        "(\n"
        "  sessions: (visibility: all),\n"
        f"  limits: (rate: (per_sec: {per_sec}, burst: {burst}, mode: reject, "
        f"max_wait_ms: {max_wait_ms}, max_queued: {max_queued})),\n"
        ")\n"
    )


def pace_scope(per_sec=2, burst=1, max_wait_ms=5000, max_queued=64):
    write_scope(
        "(\n"
        "  sessions: (visibility: all),\n"
        f"  limits: (rate: (per_sec: {per_sec}, burst: {burst}, mode: pace, "
        f"max_wait_ms: {max_wait_ms}, max_queued: {max_queued})),\n"
        ")\n"
    )


def connect():
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(30)
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
    line = f.readline()
    reply = json.loads(line)
    if "result" not in reply:
        raise RuntimeError(f"handshake refused: {reply}")
    return s, f


def call(f, method, params):
    frame = {"method": method, "params": params, "scope": SCOPE}
    f.write((json.dumps(frame) + "\n").encode())
    f.flush()
    return json.loads(f.readline())


def error_of(reply):
    return reply.get("error") or {}


def control(f, session_id):
    """A control-class call. The pointer tools are implemented (input slice); with a session
    id that does not exist the answer is `session_not_found` after dispatch -- the point is
    that the frame reached dispatch and spent a token."""
    return call(f, "pointer_move", {"session_id": session_id, "x": 1, "y": 1})


def is_rate_limited(reply):
    return error_of(reply).get("code") == "rate_limited"


def reached_dispatch(reply):
    """Past the bucket gate and into dispatch: the tool ran and answered for the (absent)
    session. `not implemented yet` was the old pass-through and is no longer produced."""
    return error_of(reply).get("code") == "session_not_found"


def retry_hint_ms(reply):
    hint = error_of(reply).get("hint") or ""
    prefix = "retry after "
    if not hint.startswith(prefix) or not hint.endswith(" ms"):
        return None
    try:
        return int(hint[len(prefix) : -len(" ms")])
    except ValueError:
        return None


# ---------------------------------------------------------------------------
# Fixture: the ids this leg drives must be ones the manager *knows*. Since #117/#118 the
# bucket is keyed only for an id that can name a session (live registry or durable history);
# with no compositor on this bench, a seeded `session.created` line is what a previously
# closed session leaves behind.
# ---------------------------------------------------------------------------
for session_id in ("sess-a", "sess-b", "sess-p", "sess-w", "sess-flood"):
    known_session.seed(DATA, session_id)
note("seeded closed-session records for the five ids this leg drives")

# ---------------------------------------------------------------------------
# [A] reject: burst 2, then a refusal carrying the retry hint
# ---------------------------------------------------------------------------
reject_scope()
s, f = connect()
try:
    a1 = control(f, "sess-a")
    a2 = control(f, "sess-a")
    a3 = control(f, "sess-a")
    check("[A1] first control call passes the gate", reached_dispatch(a1), json.dumps(a1))
    check("[A2] second control call passes (burst 2)", reached_dispatch(a2), json.dumps(a2))
    check("[A3] third control call is rate_limited", is_rate_limited(a3), json.dumps(a3))
    hint = retry_hint_ms(a3)
    check(
        "[A4] refusal carries a numeric retry-after hint",
        hint is not None and hint >= 1,
        f"hint={error_of(a3).get('hint')!r}",
    )

    # -----------------------------------------------------------------------
    # [B] reads and capture are exempt while the bucket is empty
    # -----------------------------------------------------------------------
    b1 = call(f, "session_status", {"session_id": "sess-a"})
    b2 = call(f, "screenshot", {"session_id": "sess-a"})
    check(
        "[B1] session_status is exempt from the bucket",
        error_of(b1).get("code") == "session_not_found",
        json.dumps(b1),
    )
    check(
        "[B2] screenshot is exempt from the bucket",
        error_of(b2).get("code") == "session_not_found",
        json.dumps(b2),
    )

    # -----------------------------------------------------------------------
    # [C] buckets are per (scope, session)
    # -----------------------------------------------------------------------
    c1 = control(f, "sess-b")
    check(
        "[C1] a different session has its own burst",
        reached_dispatch(c1),
        json.dumps(c1),
    )

    # -----------------------------------------------------------------------
    # [D] refill after one second at per_sec 1
    # -----------------------------------------------------------------------
    time.sleep(1.2)
    d1 = control(f, "sess-a")
    check("[D1] a refilled bucket passes again", reached_dispatch(d1), json.dumps(d1))
finally:
    s.close()

# ---------------------------------------------------------------------------
# [E] scope file re-read per call: pace waits instead of refusing
# ---------------------------------------------------------------------------
pace_scope(per_sec=2, burst=1, max_wait_ms=5000)
s, f = connect()
try:
    e1 = control(f, "sess-p")
    start = time.monotonic()
    e2 = control(f, "sess-p")
    elapsed = time.monotonic() - start
    check("[E1] paced call passes after its wait", reached_dispatch(e2), json.dumps(e2))
    check(
        "[E2] the scope file was re-read per call (pace took effect, no restart)",
        e1 is not None and 0.30 <= elapsed <= 2.0,
        f"elapsed={elapsed:.3f}s want ~0.50s",
    )
finally:
    s.close()

# ---------------------------------------------------------------------------
# [F] pace + max_wait_ms bound refuses instead of parking
# ---------------------------------------------------------------------------
pace_scope(per_sec=1, burst=1, max_wait_ms=20)
s, f = connect()
try:
    f1 = control(f, "sess-w")
    start = time.monotonic()
    f2 = control(f, "sess-w")
    elapsed = time.monotonic() - start
    check("[F1] first call passes", reached_dispatch(f1), json.dumps(f1))
    check("[F2] a wait past max_wait_ms refuses", is_rate_limited(f2), json.dumps(f2))
    check(
        "[F3] the refused wait did not park the caller",
        elapsed < 0.5,
        f"elapsed={elapsed:.3f}s",
    )
finally:
    s.close()

# ---------------------------------------------------------------------------
# [G] concurrent flood: bounded queue degrades to refusals, not a parked army
# ---------------------------------------------------------------------------
pace_scope(per_sec=1, burst=1, max_wait_ms=5000, max_queued=2)
results = []
lock = threading.Lock()


def flood(i):
    try:
        s, f = connect()
        try:
            start = time.monotonic()
            reply = control(f, "sess-flood")
            elapsed = time.monotonic() - start
        finally:
            s.close()
    except Exception as exc:  # noqa: BLE001 - a dead connection is itself a result
        reply, elapsed = {"transport_error": str(exc)}, None
    with lock:
        results.append((i, reply, elapsed))


threads = [threading.Thread(target=flood, args=(i,)) for i in range(6)]
started = time.monotonic()
for t in threads:
    t.start()
for t in threads:
    t.join()
total = time.monotonic() - started

refused = [r for r in results if r[1] is not None and is_rate_limited(r[1])]
passed = [r for r in results if r[1] is not None and reached_dispatch(r[1])]
other = [r for r in results if r[1] is not None and not is_rate_limited(r[1]) and not reached_dispatch(r[1])]
elapsed_max = max((r[2] for r in results if r[2] is not None), default=0.0)

check(
    "[G1] at least one concurrent call passes the gate",
    len(passed) >= 1,
    f"passed={len(passed)} refused={len(refused)} other={len(other)}",
)
check(
    "[G2] the queue bound refuses rather than parking the whole flood",
    len(refused) >= 2,
    f"passed={len(passed)} refused={len(refused)} other={len(other)}",
)
check(
    "[G3] every answer arrives inside the parked wait bound",
    elapsed_max <= 2.5,
    f"total={total:.3f}s max_call={elapsed_max:.3f}s",
)
if other:
    note("unexpected answers in the flood: " + json.dumps([r[1] for r in other])[:400])
if len(refused) == 0:
    note(
        "no refusal under six concurrent calls: either dispatch serialized the connections "
        "or the queue bound did not engage -- both are findings at this tip"
    )

print(f"FAILS={fails}")
sys.exit(min(fails, 120))
