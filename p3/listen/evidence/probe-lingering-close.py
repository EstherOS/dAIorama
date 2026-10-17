#!/usr/bin/env python3
"""A refused request whose body is already in flight keeps its refusal.

A client without `Expect: 100-continue` starts bursting the body immediately. If the server
writes the refusal and closes while that body is still arriving, the close turns into a reset
and the refusal's body is lost — the client gets the head and zero body bytes. The bridge now
lingers: a bounded drain (<= 64 KiB, <= 100 ms) before the FIN, only when the request carried
no Expect.

Three sizes: 256 KiB and 1 MiB are controls (usually intact either way); 4 MiB is past this
box's socket buffers and is the case that loses the body without the lingering close.

Run: python3 p3/listen/evidence/probe-lingering-close.py --bin PATH [--label before|after]
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


def read_response(sock, limit=10.0):
    """Read one response; returns (status_line, body_bytes)."""
    start = time.monotonic()
    sock.settimeout(0.5)
    buf = b""
    while b"\r\n\r\n" not in buf:
        if time.monotonic() - start > limit:
            break
        try:
            chunk = sock.recv(4096)
        except socket.timeout:
            continue
        except OSError:
            break
        if not chunk:
            break
        buf += chunk
    head, _, body = buf.partition(b"\r\n\r\n")
    status = head.split(b"\r\n", 1)[0].decode("latin-1", "replace") if head else ""
    length = 0
    for line in head.split(b"\r\n")[1:]:
        name, _, value = line.partition(b":")
        if name.strip().lower() == b"content-length":
            length = int(value.strip() or 0)
    while len(body) < length:
        try:
            chunk = sock.recv(4096)
        except socket.timeout:
            continue
        except OSError:
            break
        if not chunk:
            break
        body += chunk
    return status, body[:length] if length else body


def probe(size, port, token, host):
    body = b"x" * size
    sock = socket.create_connection(("127.0.0.1", port), timeout=10.0)
    head = (f"POST /mcp HTTP/1.1\r\nHost: {host}\r\nDaiorama-Token: {token}\r\n"
            f"Content-Length: {size}\r\n\r\n").encode()
    send_error = []

    def sender():
        try:
            sock.sendall(head + body)
        except OSError as exc:
            send_error.append(type(exc).__name__)

    thread = threading.Thread(target=sender, daemon=True)
    thread.start()
    status, payload = read_response(sock)
    thread.join(timeout=2.0)
    sock.close()
    code = None
    try:
        code = json.loads(payload)["error"]["data"]["code"]
    except Exception:
        pass
    return status, payload, code, (send_error[0] if send_error else "clean")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default=os.path.join(os.path.dirname(__file__), "..", "..", "..",
                                                 "target", "debug", "daiorama"))
    ap.add_argument("--port", type=int, default=18760)
    ap.add_argument("--label", default="")
    args = ap.parse_args()
    binary = os.path.abspath(args.bin)
    if not os.path.exists(binary):
        sys.exit(f"no binary at {binary}")
    root = tempfile.mkdtemp(prefix="dllc.", dir="/tmp")
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
        host = f"127.0.0.1:{port}"
        print(f"bin={os.path.basename(binary)} label={args.label or '-'}\n"
              f"port={port} manager_pid={mgr.pid} bridge_pid={br.pid}\n")

        print("=== a refused request whose body is already in flight ===")
        fails = 0
        for size, expect_body in ((256 * 1024, True), (1024 * 1024, True), (4 * 1024 * 1024, True)):
            status, payload, code, sender = probe(size, port, token, host)
            ok = ("200" in status) and (len(payload) > 0) and (code in ("token_required",
                                                                       "scope_auth_required"))
            if size == 4 * 1024 * 1024:
                print(f"[{'OK' if ok else 'FAIL'}]   4 MiB burst :: {status!r} "
                      f"body_bytes={len(payload)} code={code!r} sender={sender}")
            else:
                print(f"[{'INFO' if ok else 'FAIL'}] {size // 1024} KiB burst :: {status!r} "
                      f"body_bytes={len(payload)} code={code!r} sender={sender}")
            if not ok:
                fails += 1
        print(f"\nFAILS={fails}")
        sys.exit(0 if fails == 0 else 1)
    finally:
        for proc in procs:
            proc.kill()
        subprocess.run(["rm", "-rf", root], check=False)


if __name__ == "__main__":
    main()
