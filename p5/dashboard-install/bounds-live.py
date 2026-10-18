#!/usr/bin/env python3
"""Live leg for the unpacked-artifact bounds: real binary, real gzipped tars.

Proves: (a) an artifact whose declared members expand past 128 MiB is refused with
the pinned string and creates no module tree; (b) an artifact with more than 4,096
entries is refused the same way; (c) a real artifact still installs and `active`
resolves to one slug.
"""
import hashlib
import io
import os
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path

BIN = Path(sys.argv[1]).resolve()
MOD = Path(sys.argv[2]).resolve()
root = Path(tempfile.mkdtemp(prefix="d5bounds-"))
env = dict(os.environ)
env["XDG_CONFIG_HOME"] = str(root / "config")
env["XDG_DATA_HOME"] = str(root / "data")
env["XDG_RUNTIME_DIR"] = str(root / "run")
env["TMPDIR"] = str(root)
for sub in ("config/daiorama/default", "data", "run", "art"):
    (root / sub).mkdir(parents=True, exist_ok=True)
(root / "config/daiorama/default/config.ron").write_text("(version: 1, limits: (mode: off))\n")
parent = root / "data/daiorama/default/modules/daiorama-dashboard"

fails = 0


def ok(label, detail=""):
    print(f"[OK]   {label}" + (f" ({detail})" if detail else ""))


def bad(label, detail):
    global fails
    fails += 1
    print(f"[FAIL] {label} :: {detail}")


def sha(path):
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def make_tar(path, members):
    """members: list of (name, bytes|None, size_override)."""
    with tarfile.open(path, "w:gz") as tf:
        for name, data, size in members:
            info = tarfile.TarInfo(name)
            info.mode = 0o644
            if data is None:
                info.size = size
                tf.addfile(info, io.BytesIO(b"\0" * size))
            else:
                info.size = len(data)
                tf.addfile(info, io.BytesIO(data))


def run_install(artifact, *extra):
    cmd = [str(BIN), "dashboard", "install", "--artifact", str(artifact),
           "--sha256", sha(artifact), *extra]
    proc = subprocess.run(cmd, env=env, capture_output=True, text=True)
    return proc.returncode, (proc.stdout + proc.stderr).strip()


def tree_state():
    return "ABSENT" if not parent.exists() else sorted(p.name for p in parent.iterdir())


# ---- (a) 129 MiB of declared members (gzip keeps the file small) -------------
big = root / "art" / "daiorama-dashboard-0.1.0-x86_64-unknown-linux-gnu.tar.gz"
make_tar(big, [("daiorama-dashboard", b"#!/bin/sh\n", None)] +
         [(f"assets/blob-{i}", None, 4 * 1024 * 1024) for i in range(33)])
rc, text = run_install(big)
if rc == 0:
    bad("(a) 132 MiB artifact refused", f"exited 0: {text}")
elif "artifact expands beyond 128 MiB; refusing" not in text:
    bad("(a) 132 MiB artifact refused", text)
elif tree_state() != "ABSENT":
    bad("(a) no module tree", str(tree_state()))
else:
    ok("(a) 132 MiB artifact refused, no module tree", f"{big.stat().st_size} bytes on disk")

# ---- (b) 4,097 entries ------------------------------------------------------
many = root / "art" / "daiorama-dashboard-0.2.0-x86_64-unknown-linux-gnu.tar.gz"
make_tar(many, [("daiorama-dashboard", b"#!/bin/sh\n", None)] +
         [(f"assets/f-{i}", b"", None) for i in range(4096)])
rc, text = run_install(many)
if rc == 0:
    bad("(b) 4,097-entry artifact refused", f"exited 0: {text}")
elif "artifact carries more than 4096 entries; refusing" not in text:
    bad("(b) 4,097-entry artifact refused", text)
elif tree_state() != "ABSENT":
    bad("(b) no module tree", str(tree_state()))
else:
    ok("(b) 4,097-entry artifact refused, no module tree")

# ---- (c) the real artifact still installs ----------------------------------
good = root / "art" / "daiorama-dashboard-0.1.0-x86_64-unknown-linux-gnu.tar.gz"
make_tar(good, [("daiorama-dashboard", MOD.read_bytes(), None),
                ("assets/index.html", b"<!doctype html><title>probe</title>\n", None)])
rc, text = run_install(good)
link = parent / "active"
if rc != 0:
    bad("(c) real artifact installs", text)
elif not link.is_symlink() or link.resolve().name != "0.1.0":
    bad("(c) active resolves to one slug", f"{link} -> {os.readlink(link) if link.is_symlink() else '?'}")
else:
    ok("(c) real artifact installs", text.replace("\n", " | "))

# ---- (d) the core execs the installed module ------------------------------
proc = subprocess.run([str(BIN), "dashboard", "connections", "list"], env=env,
                      capture_output=True, text=True)
if proc.returncode == 0:
    ok("(d) core execs the installed module", (proc.stdout + proc.stderr).strip()[:60])
else:
    bad("(d) core execs the installed module", (proc.stdout + proc.stderr).strip()[:200])

print(f"FAILS={fails}")
print("VERDICT=" + ("PASS" if fails == 0 else "FAIL"))
sys.exit(1 if fails else 0)
