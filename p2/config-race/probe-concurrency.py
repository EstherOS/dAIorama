#!/usr/bin/env python3
"""Adversarial raw-socket probes v4 against a live daiorama manager (unix socket).

New class vs v3: true concurrency.
  A. parallel workers, mixed request classes, per-thread unique echo names
     (cross-talk detector) + fresh-connection liveness under load
  B. fd churn (200 bare + 200 handshaked connections) + liveness after
  C. config lost-update race: two concurrent RPC writers, distinct keys,
     final-state assertion + monotonicity monitors (read-modify-write race)
  D. config race, cross-process: CLI `config set` process loop vs RPC set loop
  E. torn read: `config get` hammered during a write loop; count failures

Run against a manager started off-mode (limits.mode = off); protocol-level only.
Usage: DAIORAMA_BIN=<built daiorama> probe-concurrency.py <socket> [pid] [label-filter]
"""
import json
import os
import socket
import subprocess
import sys
import threading
import time

SOCK = sys.argv[1]
PID = int(sys.argv[2]) if len(sys.argv) > 2 else None
FILTER = sys.argv[3] if len(sys.argv) > 3 else None
BIN = os.environ["DAIORAMA_BIN"]
INSTANCE = os.environ.get("DAIORAMA_INSTANCE", "probe4")

HS_OK = {"method": "handshake", "params": {"protocol_version": "0.1.0", "min_supported": "0.1.0"}}
STATUS = {"method": "status", "params": None}
LS = {"method": "list_sessions", "params": None}
GV = {"method": "get_view", "params": {"session_id": "s-1"}}


def fd_count():
    if PID is None:
        return None
    try:
        return len(os.listdir(f"/proc/{PID}/fd"))
    except OSError:
        return None


def connect(timeout=10):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    s.connect(SOCK)
    return s


class Conn:
    """One connection with a buffered line reader (leftovers preserved)."""

    def __init__(self, timeout=20):
        self.s = connect(timeout)
        self.buf = b""

    def send(self, obj):
        if isinstance(obj, bytes):
            self.s.sendall(obj)
        else:
            self.s.sendall(json.dumps(obj).encode() + b"\n")

    def recvline(self):
        while b"\n" not in self.buf:
            try:
                chunk = self.s.recv(65536)
            except socket.timeout:
                return "<TIMEOUT>"
            if not chunk:
                return None
            self.buf += chunk
        line, self.buf = self.buf.split(b"\n", 1)
        return line.decode(errors="replace")

    def handshake(self):
        self.send(HS_OK)
        return self.recvline()

    def close(self):
        try:
            self.s.close()
        except OSError:
            pass


def clip(resp, n=200):
    if resp is None:
        return "EOF"
    return resp[:n] + ("..." if len(resp) > n else "")


def probe(label, fn):
    if FILTER and FILTER not in label:
        return
    print(f"\n--- {label} ---")
    try:
        fn()
    except Exception as e:  # noqa: BLE001
        print(f"PROBE EXCEPTION: {type(e).__name__}: {e}")


# --- A: parallel workers, mixed classes, unique echo names ------------------
def a_parallel_crosstalk():
    N_WORKERS, N_REQS = 24, 40
    errors = []
    done = threading.Event()
    lat = []

    def worker(tid):
        try:
            c = Conn(timeout=30)
            r = c.handshake()
            if r is None or '"result"' not in r:
                errors.append(f"w{tid}: handshake failed: {clip(r)}")
                return
            for i in range(N_REQS):
                cls = i % 4
                if cls == 0:
                    c.send(STATUS)
                    r = c.recvline()
                    if r is None or '"pid"' not in r:
                        errors.append(f"w{tid}#{i}: status bad: {clip(r)}")
                        return
                elif cls == 1:
                    name = f"frobnicate-{tid}-{i}"
                    c.send({"method": name, "params": None})
                    r = c.recvline()
                    if r is None or name not in r or "bad_request" not in r:
                        errors.append(f"w{tid}#{i}: unknown-echo bad: {clip(r)}")
                        return
                elif cls == 2:
                    c.send(GV)
                    r = c.recvline()
                    if r is None or '"internal"' not in r:
                        errors.append(f"w{tid}#{i}: get_view bad: {clip(r)}")
                        return
                else:
                    c.send(LS)
                    r = c.recvline()
                    if r is None or '"result"' not in r:
                        errors.append(f"w{tid}#{i}: list bad: {clip(r)}")
                        return
            c.close()
        except Exception as e:  # noqa: BLE001
            errors.append(f"w{tid}: EXC {type(e).__name__}: {e}")

    threads = [threading.Thread(target=worker, args=(t,)) for t in range(N_WORKERS)]
    for t in threads:
        t.start()

    # liveness while the flood runs
    time.sleep(0.05)
    for _ in range(5):
        t0 = time.time()
        c = Conn(timeout=10)
        r = c.handshake()
        ok = r is not None and '"result"' in r
        lat.append((time.time() - t0, ok))
        c.close()
        time.sleep(0.05)

    for t in threads:
        t.join()
    done.set()
    print(f"workers={N_WORKERS} x {N_REQS} reqs; errors={len(errors)}")
    for e in errors[:10]:
        print("  ", e)
    for i, (dt, ok) in enumerate(lat):
        print(f"liveness probe {i}: {dt * 1000:.1f} ms ok={ok}")
    if not errors and all(ok for _, ok in lat):
        print(">>> no cross-talk; all responses matched; live under load <<<")


# --- B: fd churn -------------------------------------------------------------
def b_fd_churn():
    base = fd_count()
    print(f"fds baseline={base}")
    for _ in range(200):
        s = connect(timeout=5)
        s.close()
    mid = fd_count()
    time.sleep(0.3)
    mid2 = fd_count()
    print(f"after 200 bare connect/close: {mid} -> {mid2}")
    for _ in range(200):
        s = connect(timeout=5)
        s.sendall((json.dumps(HS_OK) + "\n").encode())
        try:
            s.recv(65536)
        except socket.timeout:
            pass
        s.close()
    time.sleep(0.5)
    end = fd_count()
    print(f"after 200 handshaked connect/close: {end}")
    c = Conn(timeout=5)
    r = c.handshake()
    print("liveness after churn:", clip(r, 80))
    c.close()


# --- C: config lost-update race (RPC vs RPC), monotonicity monitor -----------
def _cfg_get(c, key):
    c.send({"method": "config_get", "params": {"key": key}})
    return c.recvline()


KEY_A, KEY_B = "sessions.max_global", "history.segments"


def _monitor(key, stop, viol, errs):
    """Reads `key` as fast as possible; any read showing a value below the running
    max is a lost-update event caught in the act (a clobbering stale save)."""
    c = Conn(timeout=30)
    c.handshake()
    mx = -1
    n = 0
    while not stop.is_set():
        c.send({"method": "config_get", "params": {"key": key}})
        r = c.recvline()
        n += 1
        if r is None:
            errs.append(f"{key}: monitor EOF after {n}")
            break
        if '"result"' in r:
            try:
                v = int(json.loads(r)["result"]["value"])
            except (KeyError, TypeError, ValueError):
                continue
            if v < mx:
                viol.append((key, mx, v))
            mx = max(mx, v)
        else:
            errs.append(f"{key}: {clip(r, 120)}")
    c.close()


def _writer(key, base, iters, barrier, errs):
    c = Conn(timeout=60)
    c.handshake()
    barrier.wait()
    for i in range(iters):
        c.send({"method": "config_set", "params": {"key": key, "value": str(base + i)}})
        r = c.recvline()
        if r is None or '"result"' not in r:
            errs.append(f"{key} #{i}: {clip(r)}")
            return
    c.close()


def _race_rounds(rounds, iters, header, offset=0):
    print(header)
    lost = 0
    for rnd in range(rounds):
        # Per-round ascending bases: a round only ever writes values above every earlier
        # round's (and above any earlier section's, via `offset`), so any monitor regression
        # is a genuine clobber, not a round-boundary reset.
        base_a, base_b = 200 + offset + rnd * 1000, 20 + offset + rnd * 1000
        barrier = threading.Barrier(2)
        stop = threading.Event()
        viol, errs = [], []
        wt1 = threading.Thread(target=_writer, args=(KEY_A, base_a, iters, barrier, errs))
        wt2 = threading.Thread(target=_writer, args=(KEY_B, base_b, iters, barrier, errs))
        mt1 = threading.Thread(target=_monitor, args=(KEY_A, stop, viol, errs))
        mt2 = threading.Thread(target=_monitor, args=(KEY_B, stop, viol, errs))
        mt1.start()
        mt2.start()
        wt1.start()
        wt2.start()
        wt1.join()
        wt2.join()
        time.sleep(0.2)  # let monitors drain the last writes
        stop.set()
        mt1.join()
        mt2.join()
        c = Conn(timeout=10)
        c.handshake()
        got_a = _cfg_get(c, KEY_A)
        got_b = _cfg_get(c, KEY_B)
        c.close()
        ok_a = str(base_a + iters - 1) in got_a
        ok_b = str(base_b + iters - 1) in got_b
        if viol or not (ok_a and ok_b):
            lost += 1
        print(
            f"round {rnd}: violations={len(viol)} final {'OK' if ok_a and ok_b else 'LOST'} "
            f"(A want {base_a + iters - 1}, B want {base_b + iters - 1})"
        )
        for v in viol[:4]:
            print(f"    monitor saw {v[0]} regress {v[1]} -> {v[2]}")
        for e in errs[:4]:
            print("    err:", e)
    verb = f"{lost}/{rounds} rounds showed lost updates" if lost else "no lost updates observed"
    print(f">>> {verb} <<<")


def c_config_race():
    _race_rounds(4, 250, f"small file: 4 rounds x 250 iters each, monitors on both keys")


def c2_config_race_wide():
    # Widen the read-modify-write window: a ~256 KiB view.advertise_base makes every
    # load/save take milliseconds, so overlapping writers straddle near-deterministically.
    c = Conn(timeout=30)
    c.handshake()
    big = "x" * (256 * 1024)
    c.send({"method": "config_set", "params": {"key": "view.advertise_base", "value": big}})
    r = c.recvline()
    print("set view.advertise_base big:", "ok" if r and '"result"' in r else clip(r))
    c.close()
    _race_rounds(2, 40, "wide file (~256 KiB): 2 rounds x 40 iters each", offset=10000)
    c = Conn(timeout=30)
    c.handshake()
    c.send({"method": "config_set", "params": {"key": "view.advertise_base", "value": "unset"}})
    c.recvline()
    c.close()


# --- D: cross-process race (CLI local set vs RPC set) ------------------------
def d_cross_process_race():
    ITERS = 30
    KEY_A, KEY_B = "sessions.max_global", "history.segments"
    env = dict(os.environ)

    def rpc_writer(errs, barrier):
        c = Conn(timeout=30)
        c.handshake()
        barrier.wait()
        for i in range(ITERS):
            c.send({"method": "config_set", "params": {"key": KEY_B, "value": str(400 + i)}})
            r = c.recvline()
            if r is None or '"result"' not in r:
                errs.append(f"rpc #{i}: {clip(r)}")
                return
        c.close()

    def cli_writer(errs, barrier):
        barrier.wait()
        for i in range(ITERS):
            r = subprocess.run(
                [BIN, "--instance", INSTANCE, "config", "set", KEY_A, str(500 + i)],
                env=env, capture_output=True, text=True, timeout=30,
            )
            if r.returncode != 0:
                errs.append(f"cli #{i}: rc={r.returncode} {(r.stderr or r.stdout).strip()[:120]}")
                return

    barrier = threading.Barrier(2)
    errs = []
    t1 = threading.Thread(target=rpc_writer, args=(errs, barrier))
    t2 = threading.Thread(target=cli_writer, args=(errs, barrier))
    t1.start()
    t2.start()
    t1.join()
    t2.join()
    if errs:
        print("writer errors:", errs[:3])
    want_a, want_b = str(500 + ITERS - 1), str(400 + ITERS - 1)
    got_a = subprocess.run(
        [BIN, "--instance", INSTANCE, "config", "get", KEY_A],
        env=env, capture_output=True, text=True,
    ).stdout.strip()
    got_b = subprocess.run(
        [BIN, "--instance", INSTANCE, "config", "get", KEY_B],
        env=env, capture_output=True, text=True,
    ).stdout.strip()
    ok_a, ok_b = want_a in got_a, want_b in got_b
    print(f"{KEY_A} (cli writer) want={want_a} got={clip(got_a, 160)}")
    print(f"{KEY_B} (rpc writer) want={want_b} got={clip(got_b, 160)}")
    print(">>> cross-process: both kept <<<" if ok_a and ok_b else ">>> cross-process: at least one write lost <<<")


# --- E: torn read (get during write) -----------------------------------------
def e_torn_read():
    ITERS = 400
    KEY = "sessions.max_global"
    stop = threading.Event()
    counts = {"ok": 0, "err": 0}
    samples = []

    def writer():
        c = Conn(timeout=30)
        c.handshake()
        for i in range(ITERS):
            c.send({"method": "config_set", "params": {"key": KEY, "value": str(700 + i)}})
            c.recvline()
        c.close()
        stop.set()

    def reader():
        c = Conn(timeout=30)
        c.handshake()
        while not stop.is_set():
            c.send({"method": "config_get", "params": {"key": KEY}})
            r = c.recvline()
            if r is None:
                counts["err"] += 1
                samples.append("EOF")
                break
            if '"result"' in r:
                counts["ok"] += 1
            else:
                counts["err"] += 1
                if len(samples) < 5:
                    samples.append(clip(r, 160))
        c.close()

    t1 = threading.Thread(target=writer)
    t2 = threading.Thread(target=reader)
    t1.start()
    t2.start()
    t1.join()
    t2.join()
    print(f"reader: ok={counts['ok']} err={counts['err']}")
    for s in samples:
        print("  sample:", s)


probe("a parallel cross-talk + liveness", a_parallel_crosstalk)
probe("b fd churn", b_fd_churn)
probe("c config lost-update race (rpc/rpc)", c_config_race)
probe("c2 config race wide file", c2_config_race_wide)
probe("d config race cross-process (cli/rpc)", d_cross_process_race)
probe("e torn read (get during write)", e_torn_read)
print("\nall probes done")
