#!/usr/bin/env python3
"""Live leg: the dashboard UI as an operator uses it — a real browser, a real manager.

Every existing dashboard test drives the HTTP surface with raw sockets; none of them executes
`ui/app.js`. This probe loads the installed module's own page in Chromium, lets the page's JS
talk to the real manager through the module, clicks a session row and the Open view button, and
asserts the rendered DOM. A JSON/JS shape disagreement between the server and the page — the one
class of defect a raw-HTTP test cannot see — shows up here as a `?`, an error string or an
uncaught exception.

Usage: probe-ui.py --port 8760 --session <id> [--cdp 9222] [--timeout 30]
Prints [OK]/[FAIL] per assertion and `UI_FAILS=<n>`; exit code = UI_FAILS.
"""
import argparse
import base64
import hashlib
import json
import os
import re
import socket
import struct
import sys
import time
import urllib.request

FAILS = 0


def ok(label, detail=""):
    print(f"[OK]   {label}" + (f" ({detail})" if detail else ""))


def fail(label, detail):
    global FAILS
    FAILS += 1
    print(f"[FAIL] {label} :: {detail}")


def check(label, cond, detail=""):
    if cond:
        ok(label, detail)
    else:
        fail(label, detail)
    return cond


class WS:
    """Minimal RFC6455 client: masked text frames out, unmasked in, continuation reassembled."""

    def __init__(self, url, timeout=20.0):
        assert url.startswith("ws://"), url
        rest = url[len("ws://"):]
        authority, _, path = rest.partition("/")
        host, _, port = authority.partition(":")
        self.sock = socket.create_connection((host, int(port or 80)), timeout=timeout)
        self.sock.settimeout(timeout)
        key = base64.b64encode(os.urandom(16)).decode()
        request = (
            f"GET /{path} HTTP/1.1\r\nHost: {authority}\r\nUpgrade: websocket\r\n"
            f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n"
            "Sec-WebSocket-Version: 13\r\n\r\n"
        )
        self.sock.sendall(request.encode())
        head = b""
        while b"\r\n\r\n" not in head:
            chunk = self.sock.recv(4096)
            if not chunk:
                raise RuntimeError("CDP closed during handshake")
            head += chunk
        status = head.split(b"\r\n", 1)[0].decode(errors="replace")
        if "101" not in status:
            raise RuntimeError(f"CDP handshake refused: {status}")
        self.buffer = head.split(b"\r\n\r\n", 1)[1]

    def _read(self, n):
        while len(self.buffer) < n:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise RuntimeError("CDP stream ended")
            self.buffer += chunk
        out, self.buffer = self.buffer[:n], self.buffer[n:]
        return out

    def send(self, text):
        payload = text.encode()
        mask = os.urandom(4)
        header = bytearray([0x81])
        length = len(payload)
        if length < 126:
            header.append(0x80 | length)
        elif length < 65536:
            header.append(0x80 | 126)
            header += struct.pack(">H", length)
        else:
            header.append(0x80 | 127)
            header += struct.pack(">Q", length)
        header += mask
        masked = bytes(byte ^ mask[i % 4] for i, byte in enumerate(payload))
        self.sock.sendall(bytes(header) + masked)

    def recv(self):
        frames = []
        while True:
            first, second = self._read(2)
            fin = first & 0x80
            opcode = first & 0x0F
            length = second & 0x7F
            if length == 126:
                length = struct.unpack(">H", self._read(2))[0]
            elif length == 127:
                length = struct.unpack(">Q", self._read(8))[0]
            masked = second & 0x80
            mask = self._read(4) if masked else None
            body = self._read(length)
            if mask:
                body = bytes(byte ^ mask[i % 4] for i, byte in enumerate(body))
            if opcode == 0x8:
                raise RuntimeError("CDP closed")
            if opcode == 0x9:  # ping
                continue
            if opcode == 0xA:
                continue
            frames.append(body)
            if fin:
                return b"".join(frames).decode()

    def close(self):
        try:
            self.sock.close()
        except OSError:
            pass


class CDP:
    def __init__(self, port):
        target = None
        deadline = time.time() + 20
        while time.time() < deadline:
            try:
                with urllib.request.urlopen(f"http://127.0.0.1:{port}/json/list", timeout=2) as response:
                    targets = json.load(response)
                pages = [t for t in targets if t.get("type") == "page"]
                if pages:
                    target = pages[0]
                    break
            except Exception:
                pass
            time.sleep(0.3)
        if not target:
            raise RuntimeError("no CDP page target appeared")
        self.ws = WS(target["webSocketDebuggerUrl"])
        self.next_id = 0
        self.exceptions = []
        self.console = []
        self.cmd("Runtime.enable")
        self.cmd("Log.enable")
        self.cmd("Page.enable")

    def cmd(self, method, **params):
        self.next_id += 1
        message_id = self.next_id
        self.ws.send(json.dumps({"id": message_id, "method": method, "params": params}))
        while True:
            message = json.loads(self.ws.recv())
            if message.get("method") == "Runtime.exceptionThrown":
                detail = message["params"]["exceptionDetails"]
                self.exceptions.append(
                    detail.get("exception", {}).get("description")
                    or detail.get("text", "exception")
                )
                continue
            if message.get("method") == "Runtime.consoleAPICalled":
                self.console.append(
                    " ".join(str(a.get("value", "")) for a in message["params"].get("args", []))
                )
                continue
            if message.get("method") == "Log.entryAdded":
                entry = message["params"]["entry"]
                if entry.get("level") == "error":
                    self.console.append(f"{entry.get('source')}: {entry.get('text')}")
                continue
            if message.get("id") == message_id:
                if "error" in message:
                    raise RuntimeError(f"{method}: {message['error']}")
                return message.get("result", {})

    def eval(self, expression):
        result = self.cmd(
            "Runtime.evaluate",
            expression=expression,
            returnByValue=True,
            awaitPromise=True,
        )
        if result.get("exceptionDetails"):
            detail = result["exceptionDetails"]
            raise RuntimeError(
                detail.get("exception", {}).get("description") or detail.get("text", "eval failed")
            )
        return result.get("result", {}).get("value")

    def wait_for(self, expression, timeout, label):
        deadline = time.time() + timeout
        last = None
        while time.time() < deadline:
            try:
                last = self.eval(expression)
                if last:
                    return last
            except RuntimeError as err:
                last = str(err)
            time.sleep(0.25)
        return None if last is None else last


def redact_credential(text):
    """Never print a view credential: it authenticates the view front."""
    return re.sub(r"(?i)(credential=)[^&\s'\"]+", r"\1<redacted>", text or "")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=8760)
    parser.add_argument("--cdp", type=int, default=9222)
    parser.add_argument("--opened-file", default="", help="write the raw opened URL here (0600), never to stdout")
    parser.add_argument("--session", required=True)
    parser.add_argument("--timeout", type=float, default=30.0)
    args = parser.parse_args()

    page_url = f"http://127.0.0.1:{args.port}/"
    cdp = CDP(args.cdp)
    cdp.cmd("Page.navigate", url=page_url)

    loaded = cdp.wait_for(
        "document.readyState === 'complete' && !!document.getElementById('session-rows')",
        args.timeout,
        "page load",
    )
    check("the installed page loads and its DOM is built", bool(loaded), str(loaded)[:120])

    summary = cdp.wait_for(
        "(document.getElementById('summary')||{}).textContent || ''", args.timeout, "summary"
    )
    check(
        "managers render: one configured manager, reachable",
        summary == "1/1 managers reachable",
        repr(summary),
    )

    manager_row = cdp.eval(
        "(() => { const li = document.querySelector('#manager-list li');"
        " return li ? li.textContent : null; })()"
    )
    check(
        "the manager row carries the address and a live readiness",
        bool(manager_row) and "127.0.0.1:8756" in manager_row and "?" not in manager_row,
        repr(manager_row),
    )

    rows_expr = "document.querySelectorAll('#session-rows tr').length"
    count = cdp.wait_for(f"{rows_expr} > 0 && {rows_expr}", args.timeout, "session rows")
    check("the live session appears in the table", count == 1, f"rows={count!r}")

    row = cdp.eval(
        "(() => { const tr = document.querySelector('#session-rows tr');"
        " return tr ? Array.from(tr.children).map(td => td.textContent) : null; })()"
    )
    cells = row or []
    check("the row names the session id", args.session in cells, repr(cells))
    check("the row shows the label", "ui-leg" in cells, repr(cells))
    check("the row shows a running state", "running" in cells, repr(cells))
    check("the row shows the geometry", any("1280x720 @ 1" in cell for cell in cells), repr(cells))
    check(
        "a running session with no view reads `down`",
        "down" in cells,
        repr(cells),
    )

    cdp.cmd(
        "Runtime.evaluate",
        expression="window.__opened = null;"
        " window.open = (url) => { window.__opened = url; return null; };",
    )
    cdp.eval("document.querySelector('#session-rows tr').click(); true")
    detail = cdp.wait_for(
        "(document.getElementById('detail-id')||{}).textContent || ''", args.timeout, "detail"
    )
    check("clicking the row opens the detail pane", detail == args.session, repr(detail))

    history_count = cdp.wait_for(
        "document.querySelectorAll('#history li').length", args.timeout, "history"
    )
    history_ok = isinstance(history_count, int) and history_count > 0
    check("the history pane loads the session's records", history_ok,
          f"records={history_count!r}")
    history_note = cdp.eval("(document.getElementById('history-note')||{}).textContent || ''")
    check(
        "history settles on an end-of-list state",
        history_note in ("end of history", "no history for this session"),
        repr(history_note),
    )

    button_disabled = cdp.eval("document.getElementById('open-view').disabled")
    check("Open view is enabled for a running session", button_disabled is False,
          f"disabled={button_disabled!r}")

    cdp.eval("document.getElementById('open-view').click(); true")
    note = cdp.wait_for(
        "(() => { const t = (document.getElementById('view-note')||{}).textContent || '';"
        " return (t && t !== 'starting…') ? t : null; })()",
        args.timeout,
        "view note",
    )
    opened = cdp.eval("window.__opened")
    if opened:
        if args.opened_file:
            fd = os.open(args.opened_file, os.O_CREAT | os.O_WRONLY | os.O_TRUNC, 0o600)
            with os.fdopen(fd, "w") as handle:
                handle.write(opened)
        print(f"OPENED={redact_credential(opened)}")
    # Two honest answers: assets staged -> a generation and a view URL; assets absent ->
    # the pinned view_unavailable refusal rendered in the note. A silent no-op or a JS error
    # is the defect this leg exists to catch.
    opened_ok = bool(note) and (note.startswith("generation ") and "opened" in note)
    refused_ok = bool(note) and "view assets not installed" in note
    check("Open view answers in the page (a view or the pinned refusal)",
          opened_ok or refused_ok, repr(note))
    if opened_ok:
        check("the opened target is the view's own loopback address",
              bool(opened) and opened.startswith("http://127.0.0.1:"),
              redact_credential(opened or "")[:160])
        # The address `get_view` answers already carries the credential, and the answer also
        # carries it as a field: appending both put two copies of the secret in the URL.
        check("the opened target carries the credential exactly once",
              bool(opened) and opened.count("credential=") == 1,
              f"credential= x{opened.count('credential=') if opened else 0}")

    exceptions = list(cdp.exceptions)
    check("the page raised no uncaught exception", not exceptions, "; ".join(exceptions)[:300])
    errors = [line for line in cdp.console if "error" in line.lower()]
    check("the page logged no console error", not errors, "; ".join(errors)[:300])

    body = cdp.eval("document.documentElement.outerHTML")
    check("the page carries no manager token", "Daiorama-Token" not in body, "token header absent")

    cdp.ws.close()
    print(f"UI_FAILS={FAILS}")
    print("VERDICT=PASS" if FAILS == 0 else "VERDICT=FAIL")
    return FAILS


if __name__ == "__main__":
    sys.exit(main())
