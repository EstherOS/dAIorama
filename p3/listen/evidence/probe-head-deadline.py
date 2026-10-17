#!/usr/bin/env python3
"""P3 head-phase deadline, live against the real binary.

A per-read progress bound alone lets a client that writes one short head line every few
seconds hold a connection slot indefinitely: every read makes progress, so the 10 s bound
never fires. This probe measures the whole head phase:

  1. A head dribbled one line every 2 s must die at the total head deadline (~15 s),
     with nothing written — not sit until the client finishes and answers 200.
  2. A head written in one go is unaffected (200).
  3. WATCH (not fixed here): a body trickled at 1 byte / 2 s still holds the slot past the
     deadline; the per-read bound does not cover a slow-but-live body.

Run: python3 p3/listen/evidence/probe-head-deadline.py [--bin PATH] [--port 18761] [--before]
  --before  accept anything past the deadline as an observation (pre-fix raw)
"""
import argparse
import os
import socket
import subprocess
import sys
import tempfile
import threading
import time

HEAD_DEADLINE = 15.0
SLACK = 3.0
DRIBBLE = 2.0
WATCH_WINDOW = 25.0


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


def watch(sock, limit):
    """Wait up to `limit` for a response or a close: (what, elapsed, bytes)."""
    start = time.monotonic()
    sock.settimeout(limit)
    seen = b""
    while True:
        try:
            chunk = sock.recv(4096)
        except socket.timeout:
            return ("open", time.monotonic() - start, seen)
        except OSError:
            return ("closed", time.monotonic() - start, seen)
        if not chunk:
            return ("closed", time.monotonic() - start, seen)
        seen += chunk
        if b"\r\n\r\n" in seen:
            return ("answered", time.monotonic() - start, seen)


def show(label, ok, detail):
    print(f"[{'OK' if ok else 'FAIL'}]   {label} :: {detail}")
    return ok


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default=os.path.join(os.path.dirname(__file__), "..", "..", "..",
                                                 "target", "debug", "daiorama"))
    ap.add_argument("--port", type=int, default=18761)
    ap.add_argument("--before", action="store_true")
    args = ap.parse_args()
    binary = os.path.abspath(args.bin)
    if not os.path.exists(binary):
        sys.exit(f"no binary at {binary}")
    root = tempfile.mkdtemp(prefix="dlhd.", dir="/tmp")
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
        auth = (f"Host: {host}\r\nDaiorama-Token: {token}\r\n"
                f"Daiorama-Scope: operator\r\nDaiorama-Scope-Token: {secret}\r\n")
        print(f"bin={os.path.basename(binary)}\nport={port} manager_pid={mgr.pid} "
              f"bridge_pid={br.pid}\nmode={'before' if args.before else 'after'}\n")

        print("=== a head dribbled one line every 2 s ===")
        sock = socket.create_connection(("127.0.0.1", port), timeout=5.0)
        stop = threading.Event()

        def dribble():
            try:
                sock.sendall(b"POST /mcp HTTP/1.1\r\n")
            except OSError:
                return
            for i in range(12):
                if stop.wait(DRIBBLE):
                    return
                try:
                    sock.sendall(f"X-Pad-{i:02d}: 1\r\n".encode())
                except OSError:
                    return
            try:
                sock.sendall((auth + "Content-Length: 0\r\n\r\n").encode())
            except OSError:
                pass

        opened = time.monotonic()
        thread = threading.Thread(target=dribble, daemon=True)
        thread.start()
        what, _, seen = watch(sock, HEAD_DEADLINE + SLACK + 15)
        elapsed = time.monotonic() - opened
        stop.set()
        if args.before:
            ok = what == "answered" and elapsed > HEAD_DEADLINE + SLACK
            print(f"[WATCH]  the dribbled head is answered :: {what} after {elapsed:.2f}s "
                  f"(held past the {HEAD_DEADLINE:.0f}s deadline; pre-fix)")
            fails += 0 if ok else 1
        else:
            ok = what == "closed" and HEAD_DEADLINE - 1.5 <= elapsed <= HEAD_DEADLINE + SLACK
            fails += 0 if show("the dribbled head dies at the head deadline", ok,
                               f"{what} after {elapsed:.2f}s, bytes={len(seen)}") else 1
        sock.close()

        print("\n=== a head written in one go ===")
        fast = socket.create_connection(("127.0.0.1", port), timeout=5.0)
        body = b'{"jsonrpc":"2.0","id":1,"method":"initialize"}'
        fast.sendall((f"POST /mcp HTTP/1.1\r\n{auth}Content-Length: {len(body)}\r\n\r\n")
                     .encode() + body)
        status, _ = read_http(fast)
        fails += 0 if show("a normal head still answers", "200" in status, status) else 1
        fast.close()

        print("\n=== WATCH: a body trickled 1 byte / 2 s (not fixed here) ===")
        trickle = socket.create_connection(("127.0.0.1", port), timeout=5.0)
        trickle.sendall((f"POST /mcp HTTP/1.1\r\n{auth}Content-Length: 8388608\r\n\r\n").encode()
                        + b"{")
        stop = threading.Event()

        def drip():
            while not stop.is_set():
                stop.wait(DRIBBLE)
                try:
                    trickle.sendall(b"x")
                except OSError:
                    return

        opened = time.monotonic()
        thread = threading.Thread(target=drip, daemon=True)
        thread.start()
        what, elapsed, seen = watch(trickle, WATCH_WINDOW)
        stop.set()
        elapsed = time.monotonic() - opened
        held = what == "open" and elapsed >= WATCH_WINDOW - 0.5
        print(f"[WATCH]  a trickled body holds the slot :: {what} after {elapsed:.2f}s "
              f"(bytes={len(seen)}; one byte every 2 s is progress, so no bound fires)")
        fails += 0 if held else 1
        trickle.close()

        print(f"\nFAILS={fails}")
        sys.exit(0 if fails == 0 else 1)
    finally:
        for proc in procs:
            proc.kill()


if __name__ == "__main__":
    main()
