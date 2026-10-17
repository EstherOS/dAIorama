#!/usr/bin/env python3
"""Dispatch-stall probe: a slow dispatch must not stop the manager answering other calls.

Four connections each issue `create_session`; the configured compositor never becomes ready,
so every one of them sits inside dispatch for the full parent_ready_timeout (15 s) — one per
reactor worker. A fifth connection then issues handshake + `status`; if dispatch runs inline,
the fast call waits out the slow ones.

Pre-fix: status_latency_s ~= 15. Post-fix: status_latency_s < 1.
Exit 0 when the fast call answered under 2 s, 1 otherwise.
"""
import json
import os
import socket
import sys
import threading
import time

SOCK = sys.argv[1]
BIN = sys.argv[2] if len(sys.argv) > 2 else "daiorama"
LABEL = sys.argv[3] if len(sys.argv) > 3 else "run"

HS = {"method": "handshake", "params": {"protocol_version": "0.1.0", "min_supported": "0.1.0"}}
NEW = {"method": "create_session", "params": {"backend": "headless", "resolution": "800x600"}}
STATUS = {"method": "status", "params": None}


def conn(timeout=40):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    s.connect(SOCK)
    return s


class Conn:
    def __init__(self, timeout=40):
        self.sock = conn(timeout)
        self.buf = b""

    def send(self, obj):
        self.sock.sendall(json.dumps(obj).encode() + b"\n")

    def line(self):
        while b"\n" not in self.buf:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise EOFError("connection closed")
            self.buf += chunk
        line, _, self.buf = self.buf.partition(b"\n")
        return json.loads(line)

    def call(self, obj):
        self.send(obj)
        return self.line()


def slow_create(results, index):
    started = time.time()
    try:
        c = Conn()
        c.call(HS)
        answer = c.call(NEW)
        results[index] = {"elapsed_s": round(time.time() - started, 2), "answer": answer}
    except Exception as exc:  # noqa: BLE001 - the probe reports, never raises
        results[index] = {"elapsed_s": round(time.time() - started, 2), "error": repr(exc)}


def main():
    print(f"binary={BIN} label={LABEL}")
    results = [None] * 4
    threads = [
        threading.Thread(target=slow_create, args=(results, i), daemon=True) for i in range(4)
    ]
    for thread in threads:
        thread.start()
    # Let the four dispatches enter the manager before timing the fast call.
    time.sleep(0.5)

    started = time.time()
    c = Conn()
    c.call(HS)
    answer = c.call(STATUS)
    latency = time.time() - started
    err = answer.get("error")
    status = "error" if err else answer["result"]["state"]
    print(f"status_latency_s={latency:.2f} status={status}")

    for thread in threads:
        thread.join(timeout=40)
    creates = sorted(r["elapsed_s"] for r in results if r)
    print(f"create_elapsed_s={creates}")
    for r in results:
        if r and "answer" in r:
            print(f"create_answer={json.dumps(r['answer'])[:160]}")

    verdict = latency < 2.0
    print(f"VERDICT={'PASS' if verdict else 'FAIL'} fast_call_latency_s={latency:.2f}")
    return 0 if verdict else 1


if __name__ == "__main__":
    sys.exit(main())
