#!/usr/bin/env python3
"""Fixture: make an invented session id *known* to a manager that has no compositor.

`dispatch()` spends a B9 token and writes a history record only for an id that can name a
session -- `session_known()` is the live registry *or* a scope recorded in that id's durable
history (#117/#118). A live leg running in `rust:slim` has no compositor, so an id it invents
is invisible to both paths: no bucket (nothing to pace) and no record (nothing to read). That
is the product behaving as designed, but it left the pacing and history legs with nothing to
measure -- they went red at the tip while the product was right.

This fixture writes the one thing a closed session leaves behind: a `session.created` record
as the first line of its first segment. `History::recorded_scope()` reads exactly that line,
so the id becomes known with no compositor, no registry entry and no live session. The handler
still answers `session_not_found` -- which is what the legs assert -- while the gate in front
of it has something real to pace and record.

Legs filter the seeded line out of counts that are about live `tool.call` records
(`tool_calls()`), exactly as a real closed session's `session.created` line would be filtered.

Modes mirror the product (`src/history.rs`): 0700 session dir, 0600 segment.
"""
import json
import os
import time

DIR_MODE = 0o700
FILE_MODE = 0o600
SEGMENT = "seg-0000.log"


def _chmod(path, mode):
    try:
        os.chmod(path, mode)
    except OSError:
        pass


def seed(data_root, session_id, scope="operator", origin="shell", age_s=60):
    """Write a `session.created` first segment for `session_id`; return its path.

    `data_root` is the manager's instance directory (`<XDG_DATA_HOME>/daiorama/<instance>`),
    whose `history/` child is the history root. The record is dated `age_s` in the past so it
    sorts behind records the live leg is about to write.
    """
    root = os.path.join(data_root, "history")
    os.makedirs(root, exist_ok=True)
    _chmod(root, DIR_MODE)
    session_dir = os.path.join(root, session_id)
    os.makedirs(session_dir, exist_ok=True)
    _chmod(session_dir, DIR_MODE)
    record = {
        "at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(time.time() - age_s)),
        "event": "session.created",
        "session_id": session_id,
        "scope": scope,
        "origin": origin,
    }
    path = os.path.join(session_dir, SEGMENT)
    fd = os.open(path, os.O_CREAT | os.O_WRONLY | os.O_TRUNC, FILE_MODE)
    try:
        os.write(fd, (json.dumps(record) + "\n").encode())
    finally:
        os.close(fd)
    _chmod(path, FILE_MODE)
    # A fixture the manager cannot read would surface as a product failure; prove it here.
    with open(path) as fh:
        first = json.loads(fh.readline())
    if first.get("session_id") != session_id or first.get("scope") != scope:
        raise RuntimeError(f"session fixture unreadable at {path}: {first!r}")
    return path


def tool_calls(records):
    """The live records a control-call leg is about; the seeded `session.created` is not one."""
    return [record for record in records if record.get("event") == "tool.call"]
