#!/usr/bin/env python3
"""Drive one `tools/call` through the shipped `daiorama mcp` stdio bridge.

This is the harness entry point (10, "First run" step 3): a scope name in
`DAIORAMA_SCOPE`, JSON-RPC on stdin/stdout, one JSON object per line. The probe
deliberately speaks the legacy `initialize` path with no `_meta`, so the bridge
answers in its legacy envelope -- what a plain harness gets.

Usage:
  probe-input-live.py <daiorama-bin> <scope> <tool> <json-arguments>

Prints the raw result line, then `RESULT ok` / `RESULT error <detail>`.
Exit code 0 when the call answered without `isError`.
"""
import json
import os
import subprocess
import sys


def main() -> int:
    if len(sys.argv) != 5:
        print(__doc__)
        return 2
    binary, scope, tool, raw_args = sys.argv[1:5]
    arguments = json.loads(raw_args)

    env = dict(os.environ)
    env["DAIORAMA_SCOPE"] = scope
    proc = subprocess.Popen(
        [binary, "mcp"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=env,
    )

    def send(frame):
        proc.stdin.write(json.dumps(frame) + "\n")
        proc.stdin.flush()

    def read():
        line = proc.stdout.readline()
        if not line:
            err = proc.stderr.read()
            raise RuntimeError(f"bridge closed the stream; stderr={err!r}")
        return json.loads(line)

    try:
        send({"jsonrpc": "2.0", "id": 1, "method": "initialize",
              "params": {"protocolVersion": "2025-06-18"}})
        init = read()
        print("initialize:", json.dumps(init.get("result", init))[:300])

        send({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
              "params": {"name": tool, "arguments": arguments}})
        reply = read()
        print("tools/call:", json.dumps(reply)[:600])
        result = reply.get("result") or {}
        if reply.get("error"):
            print("RESULT error", json.dumps(reply["error"])[:300])
            return 1
        if result.get("isError"):
            print("RESULT error", json.dumps(result)[:300])
            return 1
        print("RESULT ok")
        return 0
    finally:
        try:
            proc.stdin.close()
        except Exception:
            pass
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except Exception:
            proc.kill()


if __name__ == "__main__":
    sys.exit(main())
