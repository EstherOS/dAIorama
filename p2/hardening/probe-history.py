#!/usr/bin/env python3
"""History live leg against a real `daiorama serve` and its real unix socket.

The unit tests in `src/history.rs` exercise the log in isolation. This probe exercises the
shipped path instead: a real manager process, real frames, records written by `dispatch()`
for control-class calls, and `list_history` read back over the same socket.

Cases:
  [A] cold manager: an empty read (`records: [], next_cursor: null`), then the fixture that
      makes this leg's ids known (`known_session`): #118 records only ids that can name a
      session, and this bench has no compositor to create one, so one seeded
      `session.created` line per id stands in for a session that closed earlier.
  [B] six control calls on one known (not live) session write six `tool.call` records;
      the read returns them newest-first, event `tool.call`, detail `{tool}` only (no
      arguments), scope/origin carried -- alongside the one seeded session.created line.
  [C] `limit: 2` pages the seven records (one seed + six calls) without gaps or
      duplicates; the last page has no cursor.
  [D] a garbage cursor is `bad_request`; a cursor without `session_id` is `bad_request`.
  [E] hostile session ids (`../../etc`, `a/b`, `..`) are `session_not_found` -- never a
      filesystem walk (P4-HISTORY-FU); an unknown but valid id is the same answer.
  [F] a cross-session read (no `session_id`) returns the newest records from every session
      and no cursor.
  [G] the live data root has the pinned modes: session dir 0700, segment file 0600.
  [H] rotation on the append path: eight seeded segments with the newest at the 4 MiB
      budget, then one live call -- the shipped path must leave eight segments (oldest
      dropped), not nine.
  [I] after rotation the new record is still readable.

Usage: probe-history.py <manager.sock> <data-root>
Prints [OK]/[FAIL] per assertion, a note per observation, and FAILS=<n>.
Exit code = FAILS (capped at 120).
"""
import json
import os
import socket
import sys

import known_session

SOCK = sys.argv[1]
DATA = sys.argv[2]
HIST = os.path.join(DATA, "history")
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
    reply = json.loads(f.readline())
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


def reached_dispatch(reply):
    """The tool ran and answered for the (absent) session. The nine control tools are
    implemented since the input slice; `not implemented yet` is no longer produced."""
    return error_of(reply).get("code") == "session_not_found"


TOOLS = [
    "pointer_move",
    "pointer_scroll",
    "pointer_click",
    "pointer_button_down",
    "pointer_button_up",
    "key_tap",
]


TOOL_PARAMS = {
    "pointer_move": {"x": 1, "y": 1},
    "pointer_scroll": {"dx": 1, "dy": 1},
    "pointer_click": {"button": "left"},
    "pointer_button_down": {"button": "left"},
    "pointer_button_up": {"button": "left"},
    "key_tap": {"key": "a"},
    "key_down": {"key": "a"},
    "key_up": {"key": "a"},
}


def control(f, method, session_id):
    """A control-class call: `dispatch()` records `tool.call` before the tool answers, and
    the tool params are the pinned per-tool shapes (05), not one generic pair."""
    return call(f, method, {"session_id": session_id, **TOOL_PARAMS[method]})


def history(f, **params):
    return call(f, "list_history", params)


def records_of(reply):
    return (reply.get("result") or {}).get("records") or []


def tools_of(records):
    out = []
    for record in records:
        detail = record.get("detail") or {}
        out.append(detail.get("tool"))
    return out


s, f = connect()
try:
    # -------------------------------------------------------------------
    # [A] cold read
    # -------------------------------------------------------------------
    a = history(f)
    result = a.get("result") or {}
    check(
        "[A1] a cold history answers an empty page",
        result.get("records") == [] and result.get("next_cursor") is None,
        json.dumps(a),
    )

    # -------------------------------------------------------------------
    # [A2] the fixture: two closed sessions, so `dispatch()` has a known id
    # to pace and record (#118). Seeded after the cold read so A1 still
    # sees an empty history root.
    # -------------------------------------------------------------------
    known_session.seed(DATA, "s-hist-1")
    known_session.seed(DATA, "s-hist-2")
    ok("[A2] seeded a closed-session record for s-hist-1 and s-hist-2", HIST)

    # -------------------------------------------------------------------
    # [B] six live control calls -> six records, newest first, no arguments
    # -------------------------------------------------------------------
    for tool in TOOLS:
        reply = control(f, tool, "s-hist-1")
        if not reached_dispatch(reply):
            fail(f"[B0] control call {tool} reached dispatch", json.dumps(reply))
    b = history(f, session_id="s-hist-1")
    records = records_of(b)
    calls = known_session.tool_calls(records)
    check("[B1] six control calls wrote six tool.call records", len(calls) == 6, f"n={len(calls)}")
    check(
        "[B2] the seeded session.created line is the only other record",
        len(records) == 7 and len(records) - len(calls) == 1,
        f"records={len(records)} calls={len(calls)}",
    )
    check(
        "[B3] the calls are newest-first",
        tools_of(calls) == list(reversed(TOOLS)),
        f"tools={tools_of(calls)}",
    )
    check(
        "[B4] every call record is a tool.call",
        all(record.get("event") == "tool.call" for record in calls),
        f"events={sorted({r.get('event') for r in calls})}",
    )
    check(
        "[B5] detail carries the tool name only (no arguments)",
        all(set((r.get("detail") or {}).keys()) == {"tool"} for r in calls),
        f"details={[r.get('detail') for r in calls][:2]}",
    )
    check(
        "[B6] scope, origin and session id are carried",
        all(
            r.get("scope") == SCOPE and r.get("origin") and r.get("session_id") == "s-hist-1"
            for r in calls
        ),
        f"scope={calls[0].get('scope') if calls else None} "
        f"origin={calls[0].get('origin') if calls else None}",
    )
    check(
        "[B7] `at` is RFC 3339 UTC",
        all(isinstance(r.get("at"), str) and r["at"].endswith("Z") for r in calls),
        f"at={calls[0].get('at') if calls else None}",
    )

    # -------------------------------------------------------------------
    # [C] cursor paging with limit 2
    # -------------------------------------------------------------------
    pages = []
    cursor = None
    for _ in range(4):
        page = history(f, session_id="s-hist-1", limit=2, **({"cursor": cursor} if cursor else {}))
        page_records = records_of(page)
        pages.append(page_records)
        cursor = (page.get("result") or {}).get("next_cursor")
        if not cursor:
            break
    flattened = [record for page_records in pages for record in page_records]
    check(
        "[C1] limit 2 pages the seven records as 2+2+2+1",
        [len(p) for p in pages] == [2, 2, 2, 1],
        f"sizes={[len(p) for p in pages]}",
    )
    check(
        "[C2] the pages concatenate to the single-page read, in order",
        flattened == records,
        f"paged={len(flattened)} single={len(records)}",
    )
    check(
        "[C3] the paged tool.calls are the six, newest-first",
        tools_of(known_session.tool_calls(flattened)) == list(reversed(TOOLS)),
        f"tools={tools_of(known_session.tool_calls(flattened))}",
    )
    check("[C4] the last page has no cursor", cursor is None, f"cursor={cursor!r}")

    # -------------------------------------------------------------------
    # [D] cursor refusals
    # -------------------------------------------------------------------
    d1 = history(f, session_id="s-hist-1", cursor="not-a-cursor!!")
    check(
        "[D1] a garbage cursor is bad_request",
        error_of(d1).get("code") == "bad_request",
        json.dumps(d1),
    )
    d2 = history(f, cursor="MA==")
    check(
        "[D2] a cursor without a session_id is bad_request",
        error_of(d2).get("code") == "bad_request",
        json.dumps(d2),
    )

    # -------------------------------------------------------------------
    # [E] hostile and unknown ids
    # -------------------------------------------------------------------
    for hostile in ("../../etc", "s-1/../../s-2", "..", "a/b"):
        reply = history(f, session_id=hostile)
        check(
            f"[E] hostile id {hostile!r} is session_not_found",
            error_of(reply).get("code") == "session_not_found",
            json.dumps(reply),
        )
    e2 = history(f, session_id="s-does-not-exist")
    check(
        "[E2] an unknown valid id is session_not_found",
        error_of(e2).get("code") == "session_not_found",
        json.dumps(e2),
    )

    # -------------------------------------------------------------------
    # [F] cross-session read
    # -------------------------------------------------------------------
    for tool in ("key_down", "key_up"):
        control(f, tool, "s-hist-2")
    f1 = history(f)
    cross = records_of(f1)
    cross_calls = known_session.tool_calls(cross)
    sessions = sorted({record.get("session_id") for record in cross})
    check(
        "[F1] a cross-session read returns both sessions' records",
        sessions == ["s-hist-1", "s-hist-2"] and len(cross_calls) == 8,
        f"sessions={sessions} calls={len(cross_calls)}",
    )
    check(
        "[F2] a cross-session read carries no cursor",
        (f1.get("result") or {}).get("next_cursor") is None,
        json.dumps((f1.get("result") or {}).get("next_cursor")),
    )
    check(
        "[F3] the second session's call is present in the cross-session read",
        "key_up" in tools_of(cross_calls),
        f"tools={tools_of(cross_calls)}",
    )
    note(
        "cross-session ordering sorts on `at`, which is second-granular: records written in "
        "the same second tie and keep readdir order. In-session order is file order."
    )

    # -------------------------------------------------------------------
    # [G] live modes
    # -------------------------------------------------------------------
    session_dir = os.path.join(HIST, "s-hist-1")
    segment = os.path.join(session_dir, "seg-0000.log")
    dir_mode = os.stat(session_dir).st_mode & 0o777
    file_mode = os.stat(segment).st_mode & 0o777
    check("[G1] the live session dir is 0700", dir_mode == 0o700, oct(dir_mode))
    check("[G2] the live segment file is 0600", file_mode == 0o600, oct(file_mode))

    # -------------------------------------------------------------------
    # [H] rotation on the append path
    # -------------------------------------------------------------------
    rot_dir = os.path.join(HIST, "s-rot")
    os.makedirs(rot_dir, exist_ok=True)
    for index in range(8):
        path = os.path.join(rot_dir, f"seg-{index:04}.log")
        if index == 7:
            # The newest segment sits at the 4 MiB budget: the next append rotates.
            with open(path, "wb") as fh:
                fh.write(b"x" * (4 * 1024 * 1024 + 1))
        else:
            with open(path, "w") as fh:
                fh.write(
                    json.dumps(
                        {
                            "at": "2026-10-17T00:00:00Z",
                            "event": "tool.call",
                            "session_id": "s-rot",
                            "scope": SCOPE,
                            "origin": "shell",
                            "detail": {"tool": "seed"},
                        }
                    )
                    + "\n"
                )
    reply = control(f, "pointer_move", "s-rot")
    check("[H0] the rotating call still passes", reached_dispatch(reply), json.dumps(reply))
    segments = sorted(name for name in os.listdir(rot_dir) if name.startswith("seg-"))
    total = sum(os.path.getsize(os.path.join(rot_dir, name)) for name in segments)
    check(
        "[H1] eight segments after rotation (pinned 8 x 4 MiB budget)",
        len(segments) == 8,
        f"segments={len(segments)} files={segments} total_bytes={total}",
    )
    check(
        "[H2] the oldest segment is the one dropped",
        "seg-0000.log" not in segments and "seg-0008.log" in segments,
        f"files={segments}",
    )
    note(
        "rotation is exercised live by seeding the newest segment at the budget; the "
        "assertion is on the file count under the live root, not on the unit-test helper"
    )

    # -------------------------------------------------------------------
    # [I] the post-rotation record is readable
    # -------------------------------------------------------------------
    i1 = history(f, session_id="s-rot", limit=5)
    rot_records = records_of(i1)
    check(
        "[I1] the post-rotation record is readable",
        bool(rot_records)
        and rot_records[0].get("event") == "tool.call"
        and (rot_records[0].get("detail") or {}).get("tool") == "pointer_move",
        f"first={rot_records[0] if rot_records else None}",
    )
finally:
    f.close()
    s.close()

print(f"FAILS={fails}")
sys.exit(min(fails, 120))
