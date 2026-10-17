#!/usr/bin/env python3
"""P3 transport limits, live against the real binary.

Two claims, both timing-bound:

  1. The 10 s progress timeout applies per read: a head line that stalls, a body chunk that
     stalls, and an idle kept-alive connection all close at ~10 s — and no earlier.
  2. The 64-connection cap closes over-cap sockets on accept, while the connections already
     held keep working.

Run: python3 p3/listen/evidence/probe-timeout.py [--bin PATH] [--port 18759]
"""
import argparse
import os
import socket
import subprocess
import sys
import tempfile
import time

PROGRESS = 10.0
SLACK = 3.0


def free_port(preferred):
    s = socket.socket()
    try:
        s.bind(("127.0.0.1", preferred))
        return preferred
    except OSError:
        s.close()
        s = socket.socket()
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
        s.close()
        return port
    finally:
        try:
            s.close()
        except OSError:
            pass


def read_http(sock, timeout=5.0):
    """Read one response; returns (status_line, body)."""
    sock.settimeout(timeout)
    buf = b""
    while b"\r\n\r\n" not in buf:
        chunk = sock.recv(4096)
        if not chunk:
            return (buf.split(b"\r\n", 1)[0].decode("latin-1", "replace"), b"")
        buf += chunk
    head, _, body = buf.partition(b"\r\n\r\n")
    length = 0
    for line in head.split(b"\r\n")[1:]:
        name, _, value = line.partition(b":")
        if name.strip().lower() == b"content-length":
            length = int(value.strip())
    while len(body) < length:
        chunk = sock.recv(4096)
        if not chunk:
            break
        body += chunk
    return (head.split(b"\r\n", 1)[0].decode("latin-1", "replace"), body)


def close_after(sock, limit):
    """Wait for the peer to close; returns (closed, elapsed, trailing_bytes)."""
    start = time.monotonic()
    sock.settimeout(limit)
    trailing = b""
    while True:
        try:
            chunk = sock.recv(4096)
        except socket.timeout:
            return (False, time.monotonic() - start, trailing)
        except OSError:
            return (True, time.monotonic() - start, trailing)
        if not chunk:
            return (True, time.monotonic() - start, trailing)
        trailing += chunk


def show(label, ok, detail):
    print(f"[{'OK' if ok else 'FAIL'}]   {label} :: {detail}")
    return ok


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default=os.path.join(os.path.dirname(__file__), "..", "..", "..",
                                                 "target", "debug", "daiorama"))
    ap.add_argument("--port", type=int, default=18759)
    args = ap.parse_args()
    binary = os.path.abspath(args.bin)
    if not os.path.exists(binary):
        sys.exit(f"no binary at {binary}")
    root = tempfile.mkdtemp(prefix="dlto.", dir="/tmp")
    env = dict(os.environ)
    env.update({
        "HOME": root,
        "XDG_CONFIG_HOME": f"{root}/config",
        "XDG_DATA_HOME": f"{root}/data",
        "XDG_RUNTIME_DIR": f"{root}/run",
        "TMPDIR": f"{root}/tmp",
    })
    for d in ("config", "data", "run", "tmp"):
        os.makedirs(f"{root}/{d}", exist_ok=True)
    for var in ("DAIORAMA_TOKEN", "DAIORAMA_SCOPE", "DAIORAMA_SCOPE_TOKEN"):
        env.pop(var, None)
    port = free_port(args.port)
    procs = []
    fails = 0
    try:
        subprocess.run([binary, "--version"], env=env, capture_output=True)
        subprocess.run([binary, "config", "setup"], env=env, capture_output=True, check=True)
        subprocess.run([binary, "config", "set", "limits.mode", "off"], env=env,
                       capture_output=True, check=True)
        mgr = subprocess.Popen([binary, "serve"], env=env,
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        procs.append(mgr)
        sock_path = f"{env['XDG_RUNTIME_DIR']}/daiorama/default/manager.sock"
        for _ in range(80):
            if os.path.exists(sock_path):
                break
            time.sleep(0.05)
        br = subprocess.Popen([binary, "mcp", "--listen", f"127.0.0.1:{port}"], env=env,
                              stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        procs.append(br)
        for _ in range(80):
            try:
                socket.create_connection(("127.0.0.1", port), timeout=0.2).close()
                break
            except OSError:
                time.sleep(0.05)
        token = open(f"{env['XDG_DATA_HOME']}/daiorama/default/token").read().strip()
        secret = open(
            f"{env['XDG_DATA_HOME']}/daiorama/default/scope-tokens/operator").read().strip()
        host = f"127.0.0.1:{port}"
        auth_headers = (f"Host: {host}\r\nDaiorama-Token: {token}\r\n"
                        f"Daiorama-Scope: operator\r\nDaiorama-Scope-Token: {secret}\r\n")
        print(f"bin={os.path.basename(binary)}\nport={port} manager_pid={mgr.pid} "
              f"bridge_pid={br.pid}\n")

        print("=== connection cap (64) ===")
        held = []
        for _ in range(64):
            held.append(socket.create_connection(("127.0.0.1", port), timeout=2.0))
        time.sleep(0.4)
        over = socket.create_connection(("127.0.0.1", port), timeout=2.0)
        closed, elapsed, _ = close_after(over, 3.0)
        fails += 0 if show("over-cap connection is closed on accept", closed and elapsed < 2.0,
                           f"closed={closed} after {elapsed:.2f}s") else 1
        body = b'{"jsonrpc":"2.0","id":1,"method":"initialize"}'
        held[0].sendall((f"POST /mcp HTTP/1.1\r\n{auth_headers}Content-Length: "
                         f"{len(body)}\r\n\r\n").encode() + body)
        status, _ = read_http(held[0])
        fails += 0 if show("a held connection still answers under the cap", "200" in status,
                           status) else 1
        for sock in held:
            sock.close()
        time.sleep(0.2)

        print("\n=== progress timeout (10 s per read) ===")
        # Idle keep-alive: one complete request, then silence.
        idle = socket.create_connection(("127.0.0.1", port), timeout=5.0)
        idle.sendall((f"POST /mcp HTTP/1.1\r\n{auth_headers}Content-Length: "
                      f"{len(body)}\r\n\r\n").encode() + body)
        status, _ = read_http(idle)
        closed, elapsed, _ = close_after(idle, PROGRESS + SLACK + 5)
        ok = closed and PROGRESS - 1.5 <= elapsed <= PROGRESS + SLACK
        fails += 0 if show("idle keep-alive closes at ~10s", ok,
                           f"{status.split()[1] if status else '?'}, then closed={closed} "
                           f"after {elapsed:.2f}s") else 1
        idle.close()

        # Head stall: a request line and one header, never terminated.
        stall = socket.create_connection(("127.0.0.1", port), timeout=5.0)
        stall.sendall(b"POST /mcp HTTP/1.1\r\nHost: " + host.encode() + b"\r\n")
        closed, elapsed, _ = close_after(stall, PROGRESS + SLACK + 5)
        ok = closed and PROGRESS - 1.5 <= elapsed <= PROGRESS + SLACK
        fails += 0 if show("stalled head closes at ~10s", ok,
                           f"closed={closed} after {elapsed:.2f}s") else 1
        stall.close()

        # Body stall: full head, promised 100 bytes, sent 10.
        partial = socket.create_connection(("127.0.0.1", port), timeout=5.0)
        partial.sendall((f"POST /mcp HTTP/1.1\r\n{auth_headers}Content-Length: 100\r\n\r\n")
                        .encode() + b"0123456789")
        closed, elapsed, _ = close_after(partial, PROGRESS + SLACK + 5)
        ok = closed and PROGRESS - 1.5 <= elapsed <= PROGRESS + SLACK
        fails += 0 if show("stalled body closes at ~10s", ok,
                           f"closed={closed} after {elapsed:.2f}s") else 1
        partial.close()

        print(f"\nFAILS={fails}")
        sys.exit(0 if fails == 0 else 1)
    finally:
        for proc in procs:
            proc.kill()
        subprocess.run(["rm", "-rf", root], check=False)


if __name__ == "__main__":
    main()
