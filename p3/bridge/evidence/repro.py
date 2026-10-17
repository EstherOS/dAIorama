#!/usr/bin/env python3
"""P3 W1 evidence: the stdio bridge against a real manager (off-mode), end to end.

Spawns a fresh manager root under /tmp, seeds an `agent` scope (own visibility, input +
capture grants, max 2 sessions), drives `daiorama mcp` over stdin and prints every raw
response line. Also shows the missing-scope refusal.

Run from the repo root:
    cargo build && python3 p3/bridge/evidence/repro.py > p3/bridge/evidence/bridge-live.txt
"""

import json
import os
import pathlib
import shutil
import subprocess
import time

BASE = pathlib.Path("/tmp/dai-p3-live")
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

setup = subprocess.run([BIN, "config", "setup"], env=env, capture_output=True, text=True)
print(f"$ daiorama config setup -> rc={setup.returncode}")
subprocess.run(
    [BIN, "config", "set", "limits.mode", "off"], env=env, check=True, capture_output=True
)

scope_dir = BASE / "config/daiorama/default/scopes"
scope_dir.mkdir(parents=True, exist_ok=True)
(scope_dir / "agent.ron").write_text(
    '(sessions: (max: Some(2), visibility: own),\n'
    " limits: (rate: (per_sec: 20, burst: 40, mode: pace)),\n"
    " audit: (store_clips: false, store_screenshots: false),\n"
    ' grant: (categories: ["input", "capture"]))\n'
)

manager = subprocess.Popen(
    [BIN, "serve"], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True
)
manager_stderr = manager.stderr
assert manager_stderr is not None

socket = BASE / "runtime/daiorama/default/manager.sock"
for _ in range(200):
    if socket.exists():
        break
    time.sleep(0.05)
else:
    raise SystemExit("manager socket did not appear")

requests = [
    {"jsonrpc": "2.0", "id": 1, "method": "server/discover", "params": {}},
    {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}},
    {
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {"name": "list_capabilities", "arguments": {}},
    },
    {
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": {"name": "list_sessions", "arguments": {}},
    },
    {
        "jsonrpc": "2.0",
        "id": 5,
        "method": "tools/call",
        "params": {"name": "screenshot", "arguments": {"session_id": "s-1"}},
    },
    {
        "jsonrpc": "2.0",
        "id": 6,
        "method": "tools/call",
        "params": {"name": "nope", "arguments": {}},
    },
    {
        "jsonrpc": "2.0",
        "id": 7,
        "method": "server/discover",
        "params": {"_meta": {"protocolVersion": "2025-06-18"}},
    },
]

bridge_env = dict(env, DAIORAMA_SCOPE="agent")
bridge = subprocess.run(
    [BIN, "mcp"],
    env=bridge_env,
    input="\n".join(json.dumps(request) for request in requests) + "\n",
    capture_output=True,
    text=True,
    timeout=30,
)

print()
print("## Bridge session (DAIORAMA_SCOPE=agent), raw stdout")
print(bridge.stdout.rstrip())
if bridge.stderr.strip():
    print()
    print("## Bridge stderr")
    print(bridge.stderr.rstrip())

print()
print("## Refusal without DAIORAMA_SCOPE")
missing = subprocess.run([BIN, "mcp"], env=env, input="", capture_output=True, text=True)
print(f"rc={missing.returncode}")
print(missing.stderr.rstrip())

manager.terminate()
try:
    manager.wait(timeout=10)
except subprocess.TimeoutExpired:
    manager.kill()
print()
print("## Manager stderr (trimmed)")
print("\n".join((manager_stderr.read() or "").splitlines()[:6]))
