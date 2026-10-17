#!/usr/bin/env python3
"""Adversarial stdio probes against `daiorama mcp` (bridge, P3 W1).

Each probe feeds raw bytes on stdin and records: exit code, stdout lines, stderr.
No manager needed for the local classes; one probe checks the unreachable path.
"""

import json
import os
import pathlib
import shutil
import subprocess

BASE = pathlib.Path("/tmp/dai-p3-probe")
BIN = str(pathlib.Path(os.environ.get("DAIORAMA_BIN", "target/debug/daiorama")).resolve())

shutil.rmtree(BASE, ignore_errors=True)
for name in ["config", "data", "runtime", "tmp"]:
    (BASE / name).mkdir(parents=True)

env = dict(os.environ)
env.update(
    HOME=str(BASE),
    XDG_CONFIG_HOME=str(BASE / "config"),
    XDG_DATA_HOME=str(BASE / "data"),
    XDG_RUNTIME_DIR=str(BASE / "runtime"),
    TMPDIR=str(BASE / "tmp"),
)
for key in ["DAIORAMA_TOKEN", "DAIORAMA_SCOPE", "DAIORAMA_SCOPE_TOKEN"]:
    env.pop(key, None)

subprocess.run([BIN, "config", "setup"], env=env, capture_output=True, check=True)
scopes = BASE / "config/daiorama/default/scopes"
scopes.mkdir(parents=True, exist_ok=True)
(scopes / "agent.ron").write_text(
    '(sessions: (max: Some(2), visibility: own),\n'
    " grant: (categories: [\"input\"]))\n"
)
env_scope = dict(env, DAIORAMA_SCOPE="agent")


def run(label, payload, timeout=20):
    proc = subprocess.run(
        [BIN, "mcp"], env=env_scope, input=payload, capture_output=True, timeout=timeout
    )
    out = proc.stdout.decode("utf-8", "replace").strip().splitlines()
    err = proc.stderr.decode("utf-8", "replace").strip().splitlines()
    print(f"=== {label}: rc={proc.returncode} stdout_lines={len(out)}")
    for line in out[:6]:
        print("  out:", line[:300])
    for line in err[:3]:
        print("  err:", line[:300])
    return proc.returncode, out, err


def jline(obj):
    return (json.dumps(obj) + "\n").encode()


run("null id on tools/list (invalid request per JSON-RPC, silence?)", jline({"jsonrpc": "2.0", "id": None, "method": "tools/list", "params": {}}))
run("missing id key on tools/list", jline({"jsonrpc": "2.0", "method": "tools/list", "params": {}}))
run("batch array of one request", jline([{"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}]))
run("missing method", jline({"jsonrpc": "2.0", "id": 2}))
run("method not a string", jline({"jsonrpc": "2.0", "id": 3, "method": 7}))
run("tools/call with array params", jline({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": []}))
run("tools/call with no params", jline({"jsonrpc": "2.0", "id": 5, "method": "tools/call"}))
run("tools/call notification (no id) must not execute", jline({"jsonrpc": "2.0", "method": "tools/call", "params": {"name": "list_sessions", "arguments": {}}}))
run("initialize with legacy protocolVersion param", jline({"jsonrpc": "2.0", "id": 6, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "legacy", "version": "0"}}}))
run("non-UTF-8 line", b'{"jsonrpc":"2.0","id":7,"method":"tools/list","params":{"x":"\xff\xfe"}}\n')
run("over-cap line (8 MiB + 1)", b'{"jsonrpc":"2.0","id":8,"method":"tools/list","params":{"x":"' + b'a' * (8 * 1024 * 1024) + b'"}}\n')
run("relay with no manager listening", jline({"jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": {"name": "list_sessions", "arguments": {}}}))
run("two calls, one bad one good (connection continues?)", jline({"jsonrpc": "2.0", "id": 10, "method": "nope"}) + jline({"jsonrpc": "2.0", "id": 11, "method": "ping"}))
