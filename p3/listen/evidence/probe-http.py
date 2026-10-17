#!/usr/bin/env python3
"""Adversarial raw-HTTP battery for `daiorama mcp --listen` (P3 W1b).

Runs a real manager off-mode plus a real network bridge, then speaks raw HTTP/1.1 over
sockets: header shapes a proxy or a hostile client can differ on (duplicate Host, whitespace
before the colon, bare-LF heads, Content-Length variants, Transfer-Encoding shapes), path
shapes, scope-name traversal, and connection-lifetime behaviour (idle and slow-body clients).

    python3 p3/listen/evidence/probe-http.py [--bin PATH] [--port 18758]

Every case prints one line: label, what came back, and (where the contract pins a shape) a
PASS/FAIL against it. Exploratory cases print WATCH so a human reads the raw shape.
"""
import argparse
import json
import os
import socket
import subprocess
import sys
import tempfile
import time

CREDS = []


def free_port(preferred):
    s = socket.socket()
    try:
        s.bind(("127.0.0.1", preferred))
        return preferred
    except OSError:
        s.close()
        s = socket.socket()
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]
    finally:
        s.close()


def read_response(sock, timeout=5.0):
    sock.settimeout(timeout)
    data = b""
    try:
        while b"\r\n\r\n" not in data:
            chunk = sock.recv(65536)
            if not chunk:
                return ("EOF_IN_HEAD", data)
            data += chunk
        head, rest = data.split(b"\r\n\r\n", 1)
        lines = head.decode("utf-8", "replace").split("\r\n")
        status = lines[0]
        headers = {}
        for line in lines[1:]:
            if ":" in line:
                k, v = line.split(":", 1)
                headers.setdefault(k.strip().lower(), []).append(v.strip())
        length = int(headers.get("content-length", ["0"])[0])
        body = rest
        while len(body) < length:
            chunk = sock.recv(65536)
            if not chunk:
                return ("EOF_IN_BODY:" + status, body)
            body += chunk
        return (status, body[:length])
    except socket.timeout:
        return ("TIMEOUT", data)


def send_raw(req, port, read=True, timeout=5.0):
    s = socket.create_connection(("127.0.0.1", port), timeout=timeout)
    s.sendall(req)
    if not read:
        return s
    try:
        return read_response(s, timeout)
    finally:
        s.close()


def post(path="/mcp", body=b'{"jsonrpc":"2.0","id":1,"method":"initialize"}', headers=None,
         host=None, port=0, version="HTTP/1.1", extra=b""):
    h = [] if headers is None else list(headers)
    if host is not None:
        h.append(("Host", host))
    lines = [f"POST {path} {version}".encode()]
    for k, v in h:
        lines.append(f"{k}: {v}".encode())
    if not any(k.lower() == "content-length" for k, _ in h):
        lines.append(f"Content-Length: {len(body)}".encode())
    raw = b"\r\n".join(lines) + b"\r\n\r\n" + body + extra
    return raw


def show(label, result, expect=None):
    status, body = result if result else ("<no-read>", b"")
    text = body.decode("utf-8", "replace")
    message = ""
    try:
        obj = json.loads(text)
        message = (obj.get("error", {}).get("data", {}) or {}).get("message", "")
        message = message or obj.get("error", {}).get("message", "")
    except ValueError:
        pass
    verdict = ""
    if expect is not None:
        ok = expect in status or expect in text
        verdict = "PASS" if ok else "FAIL"
    print(f"[{verdict or 'WATCH':4s}] {label:52s} {status:28s} {message!r}")
    return status, text


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default=os.path.join(os.path.dirname(__file__), "..", "..", "..",
                                                 "target", "debug", "daiorama"))
    ap.add_argument("--port", type=int, default=18758)
    args = ap.parse_args()
    binary = os.path.abspath(args.bin)
    if not os.path.exists(binary):
        sys.exit(f"no binary at {binary}")
    root = tempfile.mkdtemp(prefix="dlhttp.", dir="/tmp")
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
        secret = open(f"{env['XDG_DATA_HOME']}/daiorama/default/scope-tokens/operator").read().strip()
        host = f"127.0.0.1:{port}"
        auth = [("Daiorama-Token", token), ("Daiorama-Scope", "operator"),
                ("Daiorama-Scope-Token", secret)]
        print(f"bin={os.path.basename(binary)}\nport={port} manager_pid={mgr.pid} bridge_pid={br.pid}\n")

        print("=== baseline ===")
        show("valid POST /mcp initialize",
             send_raw(post(headers=auth, host=host, port=port), port), '"result"')

        print("\n=== host / header shapes ===")
        dup = post(headers=auth + [("Host", host), ("Host", "evil.example")], host=None)
        show("duplicate Host (real, evil)", send_raw(dup, port), "400")
        dup2 = post(headers=auth + [("Host", host), ("Host", host)], host=None)
        show("duplicate Host (same twice)", send_raw(dup2, port), "400")
        show("space before colon: 'Host : x'",
             send_raw(post(headers=auth + [("Host", host)], host=None).replace(
                 f"Host: {host}".encode(), f"Host : {host}".encode()), port), "400")
        show("bare-LF head (no CR)",
             send_raw(post(headers=auth, host=host).replace(b"\r\n", b"\n"), port), None)
        show("header line without colon",
             send_raw(post(headers=auth + [("Broken", "")], host=host).replace(
                 b"Broken: \r\n", b"Broken\r\n"), port), "malformed HTTP request")
        show("empty Host value",
             send_raw(post(headers=auth, host=""), port), "400")

        print("\n=== content-length shapes ===")
        body = b'{"jsonrpc":"2.0","id":2,"method":"initialize"}'
        raw = (f"POST /mcp HTTP/1.1\r\nHost: {host}\r\nContent-Length: +{len(body)}\r\n"
               f"Daiorama-Token: {token}\r\nDaiorama-Scope: operator\r\n"
               f"Daiorama-Scope-Token: {secret}\r\n\r\n").encode() + body
        show("Content-Length: +N", send_raw(raw, port), "400")
        raw = (f"POST /mcp HTTP/1.1\r\nHost: {host}\r\nContent-Length: 00{len(body)}\r\n"
               f"Daiorama-Token: {token}\r\nDaiorama-Scope: operator\r\n"
               f"Daiorama-Scope-Token: {secret}\r\n\r\n").encode() + body
        show("Content-Length: 00N", send_raw(raw, port), None)
        raw = (f"POST /mcp HTTP/1.1\r\nHost: {host}\r\nContent-Length: -1\r\n"
               f"Daiorama-Token: {token}\r\nDaiorama-Scope: operator\r\n"
               f"Daiorama-Scope-Token: {secret}\r\n\r\n").encode()
        show("Content-Length: -1", send_raw(raw, port), "400")
        raw = (f"POST /mcp HTTP/1.1\r\nHost: {host}\r\nContent-Length: {len(body)}, {len(body)}\r\n"
               f"Daiorama-Token: {token}\r\nDaiorama-Scope: operator\r\n"
               f"Daiorama-Scope-Token: {secret}\r\n\r\n").encode() + body
        show("Content-Length: N, N", send_raw(raw, port), "400")

        print("\n=== transfer-encoding / sizing ===")
        show("Transfer-Encoding: chunked",
             send_raw(post(headers=auth + [("Transfer-Encoding", "chunked")], host=host), port),
             "chunked request bodies are not supported")
        show("Transfer-Encoding: identity",
             send_raw(post(headers=auth + [("Transfer-Encoding", "identity")], host=host), port),
             "chunked request bodies are not supported")
        show("TE chunked + CL",
             send_raw(post(headers=auth + [("Transfer-Encoding", "chunked")], host=host), port),
             "chunked")
        big = (f"POST /mcp HTTP/1.1\r\nHost: {host}\r\nContent-Length: {8 * 1024 * 1024 + 1}\r\n"
               f"Daiorama-Token: {token}\r\n\r\n").encode()
        show("Content-Length MAX+1", send_raw(big, port), "request frame must be at most")

        print("\n=== target shapes ===")
        show("GET /mcp", send_raw(post(headers=auth, host=host)[:-1].replace(
            b"POST", b"GET", 1), port), "405")
        show("POST /mcp?x=1", send_raw(post(path="/mcp?x=1", headers=auth, host=host), port), "404")
        show("POST /mcp/../mcp", send_raw(post(path="/mcp/../mcp", headers=auth, host=host), port),
             "404")
        show("absolute-form target", send_raw(post(path=f"http://{host}/mcp", headers=auth,
                                                   host=host), port), "404")
        show("no Host", send_raw(post(headers=auth, host=None), port), "Host")

        print("\n=== scope names (traversal) ===")
        for scope in ("../token", "/etc/passwd", "operator/../operator", "operator\x00x",
                      "..%2ftoken", "."):
            h = [("Daiorama-Token", token), ("Daiorama-Scope", scope),
                 ("Daiorama-Scope-Token", "x")]
            try:
                r = send_raw(post(headers=h, host=host), port)
            except (UnicodeEncodeError, ValueError) as exc:
                print(f"[WATCH] scope {scope!r:26s} client-side refused: {exc}")
                continue
            show(f"scope {scope!r}", r, None)

        print("\n=== origin shapes ===")
        for origin in (f"http://{host}", f"HTTP://{host}", f"http://{host}/",
                       f"http://user@{host}", f"http://{host}@evil.example", "null",
                       f"http://{host}:1"):
            h = auth + [("Origin", origin)]
            show(f"Origin: {origin}", send_raw(post(headers=h, host=host), port), None)

        print("\n=== connection lifetime ===")
        s = socket.create_connection(("127.0.0.1", port), timeout=3)
        s.sendall(post(headers=auth + [("Connection", "close")], host=host))
        r = read_response(s, 3)
        s.settimeout(2.0)
        try:
            tail = s.recv(1)
            print(f"[WATCH] Connection: close -> {r[0]}; socket then {tail!r} (b'' = closed)")
        except socket.timeout:
            print(f"[WATCH] Connection: close -> {r[0]}; socket STILL OPEN after 2s")
        s.close()

        idle = []
        for _ in range(64):
            idle.append(socket.create_connection(("127.0.0.1", port), timeout=2))
        time.sleep(1.0)
        bridged = subprocess.run(["bash", "-c", f"ls /proc/{br.pid}/fd | wc -l"],
                                 capture_output=True, text=True).stdout.strip()
        print(f"[WATCH] 64 idle connections still open: {len(idle)}; bridge fds now {bridged}")
        for c in idle:
            c.close()

        # slow body: promise 100 bytes, send 10, wait
        s3 = socket.create_connection(("127.0.0.1", port), timeout=2)
        s3.sendall((f"POST /mcp HTTP/1.1\r\nHost: {host}\r\nContent-Length: 100\r\n"
                    f"Daiorama-Token: {token}\r\nDaiorama-Scope: operator\r\n"
                    f"Daiorama-Scope-Token: {secret}\r\n\r\n").encode() + b'{"jsonrpc"')
        time.sleep(2.0)
        s3.settimeout(1.0)
        try:
            got = s3.recv(4096)
            print(f"[WATCH] slow body after 2s: recv {got[:60]!r}")
        except socket.timeout:
            print("[WATCH] slow body after 2s: no reply, connection still held (no read timeout)")
        s3.close()

        # idle head: connect, send nothing
        s4 = socket.create_connection(("127.0.0.1", port), timeout=2)
        time.sleep(2.0)
        s4.settimeout(1.0)
        try:
            got = s4.recv(1)
            print(f"[WATCH] idle head after 2s: recv {got!r}")
        except socket.timeout:
            print("[WATCH] idle head after 2s: still open, no timeout")
        s4.close()

        print("\n=== pipelining ===")
        s5 = socket.create_connection(("127.0.0.1", port), timeout=3)
        first = post(headers=auth, host=host, body=b'{"jsonrpc":"2.0","id":21,"method":"initialize"}')
        second = post(headers=auth, host=host,
                      body=b'{"jsonrpc":"2.0","id":22,"method":"tools/list"}')
        s5.sendall(first + second)
        r1 = read_response(s5, 3)
        r2 = read_response(s5, 3)
        print(f"[WATCH] pipelined #1 {r1[0]} ids={r1[1][:60]!r}")
        print(f"[WATCH] pipelined #2 {r2[0]} ids={r2[1][:60]!r}")
        s5.close()

        print("\n=== parse-before-auth ===")
        show("no creds + malformed JSON",
             send_raw(post(body=b"not json", host=host), port), None)
        show("no creds + valid call",
             send_raw(post(body=b'{"jsonrpc":"2.0","id":31,"method":"initialize"}', host=host),
                      port), "token")
    finally:
        for p in reversed(procs):
            p.kill()
        subprocess.run(["rm", "-rf", root])


if __name__ == "__main__":
    main()
