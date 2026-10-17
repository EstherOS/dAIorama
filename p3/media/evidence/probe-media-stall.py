#!/usr/bin/env python3
"""Live battery: the `/media/<id>` refusal close on the real binary.

A media refusal is written while the client may still be bursting a body. The transport fixes
already make every refusal half-close and linger; these legs ask whether the media surface
rides that same path:

  A. refusal survives a body already in flight: `POST /media/<id>` with a body and
     `GET /media/<id>` with a wrong token and a body. The refusal body must arrive whole.
  B. a client that stops reading is cut off: one client fetches a 64 MiB artifact, stays
     silent for 20 s, then drains. The bridge must have closed the connection at the write
     progress bound (a bounded prefix delivered), not resumed the whole body when the
     client came back.

    python3 p3/media/evidence/probe-media-stall.py --bin PATH
"""
import argparse
import json
import os
import socket
import subprocess
import sys
import tempfile
import threading
import time

ID_BIG = "9" * 32
ID_OK = "a" * 32
BIG_LENGTH = 64 * 1024 * 1024  # far past any socket buffer: a non-reading client blocks the writer
BURST = 4 * 1024 * 1024

failures = 0


def report(ok, label, detail):
    global failures
    if not ok:
        failures += 1
    print(f"[{'PASS' if ok else 'FAIL'}] {label} :: {detail}")


def read_head(conn):
    buf = b""
    while b"\r\n\r\n" not in buf:
        chunk = conn.recv(4096)
        if not chunk:
            return buf
        buf += chunk
    return buf.split(b"\r\n\r\n", 1)[0]


def start_fake(root):
    """A manager that streams an artifact of `BIG_LENGTH` bytes for `ID_BIG`."""
    socket_path = os.path.join(root, "run/daiorama/default/manager.sock")
    os.makedirs(os.path.dirname(socket_path), exist_ok=True)
    srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    srv.bind(socket_path)
    srv.listen(128)

    def handle(conn):
        try:
            head = read_head(conn)
            line = head.split(b"\r\n", 1)[0].decode("latin-1")
            target = line.split()[1] if len(line.split()) > 1 else ""
            if target.endswith(ID_BIG):
                conn.sendall(
                    (
                        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\n"
                        f"Content-Length: {BIG_LENGTH}\r\n\r\n"
                    ).encode()
                )
                chunk = b"a" * 65536
                sent = 0
                while sent < BIG_LENGTH:
                    conn.sendall(chunk)
                    sent += len(chunk)
        except OSError:
            pass
        finally:
            try:
                conn.close()
            except OSError:
                pass

    def loop():
        while True:
            try:
                conn, _ = srv.accept()
            except OSError:
                return
            threading.Thread(target=handle, args=(conn,), daemon=True).start()

    threading.Thread(target=loop, daemon=True).start()
    return srv


def probe_stalled_reader(port, token):
    """Fetch a large artifact, stay silent, then drain: did the bridge cut the write?"""
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 8192)
    s.settimeout(5.0)
    s.connect(("127.0.0.1", port))
    request = (
        f"GET /media/{ID_BIG} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n"
        f"Daiorama-Token: {token}\r\nConnection: close\r\n\r\n"
    )
    s.sendall(request.encode())
    head = read_head(s)
    time.sleep(20)  # silent: a client that accepts nothing must not pin the writer
    s.settimeout(3.0)
    body = b""
    cut = False
    started = time.monotonic()
    while len(body) < BIG_LENGTH and time.monotonic() - started < 15:
        try:
            chunk = s.recv(65536)
        except (TimeoutError, socket.timeout):
            break
        except OSError:
            cut = True
            break
        if not chunk:
            cut = True
            break
        body += chunk
    s.close()
    detail = (
        f"head={head.split(b'\\r\\n', 1)[0].decode('latin-1', 'replace')!r} "
        f"after 20 s silent: received={len(body)} of {BIG_LENGTH} closed={cut}"
    )
    return cut and len(body) < BIG_LENGTH, detail


def burst_refusal(port, method, headers):
    """Send a request with a body already in flight, then read the refusal."""
    s = socket.create_connection(("127.0.0.1", port), timeout=5.0)
    s.settimeout(0.5)
    request = f"{method} /media/{ID_OK} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n"
    for name, value in headers:
        request += f"{name}: {value}\r\n"
    request += f"Content-Length: {BURST}\r\nConnection: close\r\n\r\n"
    payload = request.encode() + b"x" * BURST

    def send():
        # One sendall: the head and as much body as the kernel takes are queued together,
        # so the server's refusal meets a receive buffer that already holds body bytes.
        try:
            s.sendall(payload)
        except OSError:
            pass

    threading.Thread(target=send, daemon=True).start()
    buf = b""
    start = time.monotonic()
    while b"\r\n\r\n" not in buf and time.monotonic() - start < 5.0:
        try:
            chunk = s.recv(65536)
        except (TimeoutError, socket.timeout):
            continue
        except OSError:
            break
        if not chunk:
            break
        buf += chunk
    head, _, body = buf.partition(b"\r\n\r\n")
    length = 0
    for line in head.split(b"\r\n")[1:]:
        name, _, value = line.partition(b":")
        if name.strip().lower() == b"content-length":
            length = int(value.strip() or 0)
    deadline = time.time() + 3.0
    while len(body) < length and time.time() < deadline:
        try:
            chunk = s.recv(65536)
        except (TimeoutError, socket.timeout):
            continue
        except OSError:
            break
        if not chunk:
            break
        body += chunk
    s.close()
    status = head.split(b"\r\n", 1)[0].decode("latin-1", "replace") if head else "<none>"
    return status, body[:length]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True)
    ap.add_argument("--port", type=int, default=18776)
    args = ap.parse_args()
    binary = os.path.abspath(args.bin)
    root = tempfile.mkdtemp(prefix="dlstall.")
    env = dict(os.environ)
    env.update(
        {
            "HOME": root,
            "XDG_CONFIG_HOME": f"{root}/config",
            "XDG_DATA_HOME": f"{root}/data",
            "XDG_RUNTIME_DIR": f"{root}/run",
            "TMPDIR": f"{root}/tmp",
        }
    )
    for d in ("config", "data", "run", "tmp"):
        os.makedirs(f"{root}/{d}", exist_ok=True)
    for key in ("DAIORAMA_TOKEN", "DAIORAMA_SCOPE", "DAIORAMA_SCOPE_TOKEN"):
        env.pop(key, None)
    subprocess.run([binary, "config", "setup"], env=env, capture_output=True, check=True)
    bridge = subprocess.Popen(
        [binary, "mcp", "--listen", f"127.0.0.1:{args.port}"],
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
    )
    for _ in range(100):
        line = bridge.stderr.readline().decode(errors="replace")
        if "listening on" in line:
            break
    token = open(f"{root}/data/daiorama/default/token").read().strip()
    start_fake(root)
    print(f"bin={os.path.basename(binary)} port={args.port}")

    delivered = 0
    rounds = 4
    last = ("<none>", b"")
    for _ in range(rounds):
        status, body = burst_refusal(args.port, "POST", [])
        last = (status, body)
        if status.startswith("HTTP/1.1 405") and b"media fetches are GET only" in body:
            delivered += 1
    report(
        delivered == rounds,
        "a 405 media refusal survives a body already in flight",
        f"{delivered}/{rounds} delivered",
    )
    print(f"   last: {last[0]!r} body={last[1][:60]!r}")

    delivered = 0
    last = ("<none>", b"")
    for _ in range(rounds):
        status, body = burst_refusal(args.port, "GET", [("Daiorama-Token", "wrong")])
        last = (status, body)
        if status.startswith("HTTP/1.1 403") and b"token_required" in body:
            delivered += 1
    report(
        delivered == rounds,
        "a 403 media refusal survives a body already in flight",
        f"{delivered}/{rounds} delivered",
    )
    print(f"   last: {last[0]!r} body={last[1][:60]!r}")

    served, detail = probe_stalled_reader(args.port, token)
    report(
        served,
        "a client that stops reading is cut off, not waited out",
        detail,
    )

    bridge.terminate()
    try:
        bridge.wait(timeout=5)
    except subprocess.TimeoutExpired:
        bridge.kill()
    print(f"FAILS={failures}")
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
