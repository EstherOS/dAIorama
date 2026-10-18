#!/usr/bin/env python3
"""C2 install-script live leg at the merged tip: a real HTTP release base, a real minisign
keypair, and the shipped `packaging/release/install.sh` doing the whole decision.

The in-tree suite (`tests/install_script.rs`) stubs `minisign` on PATH and uses `--base <dir>`,
so it proves ordering but never the wire path or the crypto. This leg drives what an operator
actually runs: `curl` against an HTTP base, signature verification by the distribution
`minisign`, digest-before-placement, and the refusal set on the same bytes.

  [P] provisioned installer over HTTP: fetch -> verify -> read facts -> select row -> hash ->
      place; receipt lines, executable placement, no temp file left in the prefix;
  [C] the placed binary survives a failed second install byte-for-byte (digest mismatch);
  [R] refusals on real HTTP: untrusted signer, version mismatch, duplicate row, row-target
      mismatch, short digest, non-core kind, bad format, over-cap metadata, over-cap artifact;
  [E] the shipped installer (empty trust list) refuses before any fetch, with a dead base;
  [V] usage/validation refusals: bad version, empty prefix, unknown target, unknown option.

Usage: probe-install-live.py <install.sh> <key1.pub> <key1.sec> <key2.pub> <key2.sec>
Prints [OK]/[FAIL] per assertion plus FAILS=<n>; exit code = FAILS (capped at 120).
"""
import hashlib
import http.server
import os
import shutil
import socket
import socketserver
import subprocess
import sys
import threading

SCRIPT, KEY1_PUB, KEY1_SEC, KEY2_PUB, KEY2_SEC = sys.argv[1:6]
WORK = "/tmp/instlive"
VERSION = "0.1.0"
TARGET = "x86_64-unknown-linux-gnu"
NAME = f"daiorama-{VERSION}-{TARGET}"
PROTOCOL = "0.1.0"

FAILS = 0
OKS = 0


def report(ok, label, detail=""):
    global FAILS, OKS
    if ok:
        OKS += 1
        print(f"[OK]   {label}")
    else:
        FAILS += 1
        print(f"[FAIL] {label}" + (f" :: {detail}" if detail else ""))


def check(label, cond, detail=""):
    report(bool(cond), label, detail)


def digest(path):
    with open(path, "rb") as handle:
        return hashlib.sha256(handle.read()).hexdigest()


def free_port():
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


class Handler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, format, *args):  # keep the leg output readable
        pass


class Server(socketserver.TCPServer):
    def handle_error(self, request, client_address):
        if not isinstance(sys.exc_info()[1], BrokenPipeError):
            super().handle_error(request, client_address)


def serve(directory, port):
    handler = lambda *a, **kw: Handler(*a, directory=directory, **kw)
    httpd = Server(("127.0.0.1", port), handler)
    thread = threading.Thread(target=httpd.serve_forever, daemon=True)
    thread.start()
    return httpd


def embedded_keys(text):
    marker = "<<'TRUSTED_KEYS'\n"
    start = text.find(marker)
    if start < 0:
        raise SystemExit("install.sh has no trusted-keys here-doc")
    start += len(marker)
    end = text.find("\nTRUSTED_KEYS\n", start)
    if end < 0:
        raise SystemExit("install.sh has no trusted-keys terminator")
    return text[start:end]


def provisioned(src, dest, keys):
    text = open(src).read()
    body = embedded_keys(text)
    open(dest, "w").write(text.replace(body, keys.rstrip("\n"), 1))
    os.chmod(dest, 0o755)
    return dest


def manifest(version=VERSION, target=TARGET, sha=None, rows=1, kind="core", fmt=1):
    row = (
        '{"name":"%s","target":"%s","kind":"%s","sha256":"%s","size":1}'
        % (NAME, target, kind, sha or "0" * 64)
    )
    artifacts = ",\n".join(row for _ in range(rows))
    return (
        '{"format":%d,"version":"%s","protocol":"%s","channel":"stable",'
        '"released_at":"2026-10-18T00:00:00Z","artifacts":[%s],"assets":[],'
        '"facts":{"client_only":false,"manager_platforms":["linux"],'
        '"components":["core"]}}\n' % (fmt, version, PROTOCOL, artifacts)
    )


def release(directory, manifest_text, artifact_bytes, sign_key=None, artifact_name=None):
    os.makedirs(directory, exist_ok=True)
    path = os.path.join(directory, f"daiorama-release-{VERSION}.json")
    open(path, "w").write(manifest_text)
    signed = subprocess.run(
        ["minisign", "-Sm", path, "-s", sign_key or KEY1_SEC], capture_output=True, text=True
    )
    if signed.returncode != 0:
        raise SystemExit(f"minisign sign failed: {signed.stderr[:300]}")
    if artifact_bytes is not None:
        open(os.path.join(directory, artifact_name or NAME), "wb").write(artifact_bytes)
    return directory


def run(args, timeout=120):
    proc = subprocess.run(
        ["sh", INSTALLER] + args, capture_output=True, text=True, timeout=timeout
    )
    return proc.returncode, proc.stdout, proc.stderr


def refused(label, rc, err, needle):
    check(label, rc != 0 and needle in err,
          f"rc={rc} want {needle!r} in {err.strip()[:400]!r}")


ARTIFACT = (b'#!/bin/sh\nif [ "$1" = doctor ]; then echo "fake daiorama doctor"; exit 0; fi\n'
            b'echo fake-daiorama\n')
ARTIFACT_SHA = hashlib.sha256(ARTIFACT).hexdigest()
PAYLOAD = b"#!/bin/sh\necho tampered\n"
PAYLOAD_SHA = hashlib.sha256(PAYLOAD).hexdigest()

shutil.rmtree(WORK, ignore_errors=True)
os.makedirs(WORK)
INSTALLER = provisioned(SCRIPT, os.path.join(WORK, "install.sh"), open(KEY1_PUB).read())
SHIPPED = os.path.abspath(SCRIPT)

port = free_port()
base = f"http://127.0.0.1:{port}"
served = os.path.join(WORK, "served")
os.makedirs(served, exist_ok=True)
httpd = serve(served, port)

# ---------------------------------------------------------------- [P] happy path over HTTP
prefix = os.path.join(WORK, "prefix")
release(served, manifest(sha=ARTIFACT_SHA), ARTIFACT)
rc, out, err = run(["--version", VERSION, "--base", base, "--prefix", prefix, "--client-only"])
target_path = os.path.join(prefix, "daiorama")
check("install over HTTP exits 0", rc == 0, f"rc={rc} out={out.strip()[:300]} err={err.strip()[:400]}")
check("installed file exists", os.path.isfile(target_path), target_path)
if os.path.isfile(target_path):
    check("installed bytes are the artifact", digest(target_path) == ARTIFACT_SHA,
          digest(target_path))
    check("installed file is executable", os.access(target_path, os.X_OK),
          oct(os.stat(target_path).st_mode))
check("receipt names version/protocol/target",
      f"installed daiorama {VERSION} (protocol {PROTOCOL}) {TARGET}" in out, out.strip()[:300])
check("receipt prints the sha256", f"sha256 {ARTIFACT_SHA}" in out, out.strip()[:300])
key1_id = [line for line in open(KEY1_PUB).read().splitlines() if "key" in line][0].split()[-1]
check("receipt names the trust anchor that verified", f"signed by {key1_id}" in out,
      f"key1={key1_id} out={out.strip()[:300]!r}")
check("no temp file left in the prefix",
      os.path.isdir(prefix) and not [n for n in os.listdir(prefix) if n.startswith(".daiorama.tmp")],
      str(os.listdir(prefix)) if os.path.isdir(prefix) else "prefix absent")

# --------------------------------------------- [C] failed re-install preserves the placed file
if os.path.isfile(target_path):
    before = digest(target_path)
    release(served, manifest(sha=ARTIFACT_SHA), PAYLOAD)
    rc, out, err = run(["--version", VERSION, "--base", base, "--prefix", prefix, "--client-only"])
    refused("digest mismatch is refused", rc, err, "does not match the manifest row")
    check("placed binary unchanged after the refusal", digest(target_path) == before)
else:
    check("failed re-install preserves the placed binary", False, "nothing was placed to preserve")


# ------------------------------------------------------- [R] refusals on the same HTTP wire
def refuse_case(label, needle, directory, sign_key=None, artifact=ARTIFACT,
                artifact_name=None):
    """Rebuild `served`, run into a fresh prefix, assert the refusal and nil placement."""
    shutil.rmtree(served, ignore_errors=True)
    release(served, directory, artifact, sign_key=sign_key, artifact_name=artifact_name)
    where = os.path.join(WORK, "refuse-" + label.split()[0])
    shutil.rmtree(where, ignore_errors=True)
    rc, out, err = run(["--version", VERSION, "--base", base, "--prefix", where])
    refused(label, rc, err, needle)
    check(f"nothing placed: {label}", not os.path.exists(os.path.join(where, "daiorama")),
          str(os.listdir(where)) if os.path.isdir(where) else "absent")
    return rc, out, err


refuse_case("untrusted signer is refused", "signed by an untrusted key",
            manifest(sha=ARTIFACT_SHA), sign_key=KEY2_SEC)
key2_id = [line for line in open(KEY2_PUB).read().splitlines() if "key" in line][0].split()[-1]
shutil.rmtree(served, ignore_errors=True)
release(served, manifest(sha=ARTIFACT_SHA), ARTIFACT, sign_key=KEY2_SEC)
where = os.path.join(WORK, "refuse-keyid")
rc, out, err = run(["--version", VERSION, "--base", base, "--prefix", where, "--client-only"])
check("untrusted-key message names the presented key id", key2_id.lower() in err.lower(),
      f"key2={key2_id} err={err.strip()[:400]!r}")

refuse_case("manifest version mismatch is refused", "does not match the requested",
            manifest(version="0.1.1", sha=ARTIFACT_SHA))
refuse_case("duplicate row is refused", "duplicate release manifest row",
            manifest(sha=ARTIFACT_SHA, rows=2))
refuse_case("row target mismatch is refused", "targets",
            manifest(target="aarch64-unknown-linux-gnu", sha=ARTIFACT_SHA))
refuse_case("short digest is refused", "is not a core artifact",
            manifest(sha=ARTIFACT_SHA[:63]))
refuse_case("bad manifest format is refused", "unsupported release manifest format",
            manifest(sha=ARTIFACT_SHA, fmt=2))

# over-cap metadata: 1 MiB + 1 byte, served with a Content-Length so curl can refuse it
big_manifest = manifest(sha=ARTIFACT_SHA)
big_manifest = big_manifest[:-1] + " " * (1048576 + 1 - len(big_manifest)) + "\n"
shutil.rmtree(served, ignore_errors=True)
release(served, big_manifest, ARTIFACT)
where = os.path.join(WORK, "refuse-metacap")
rc, out, err = run(["--version", VERSION, "--base", base, "--prefix", where])
check("over-cap metadata is refused before verification",
      rc != 0 and ("cannot fetch" in err or "larger than 1048576" in err),
      f"rc={rc} err={err.strip()[:300]!r}")

# over-cap artifact: 128 MiB + 1, refused on Content-Length without the transfer
shutil.rmtree(served, ignore_errors=True)
release(served, manifest(sha=ARTIFACT_SHA), None)
with open(os.path.join(served, NAME), "wb") as handle:
    handle.truncate(128 * 1024 * 1024 + 1)
where = os.path.join(WORK, "refuse-artcap")
rc, out, err = run(["--version", VERSION, "--base", base, "--prefix", where])
check("over-cap artifact is refused", rc != 0, f"rc={rc} err={err.strip()[:300]!r}")

# ------------------------------------- [E] shipped installer: empty trust list refuses early
dead = f"http://127.0.0.1:{free_port()}"
where = os.path.join(WORK, "refuse-trust")
proc = subprocess.run(["sh", SHIPPED, "--version", VERSION, "--base", dead, "--prefix", where],
                      capture_output=True, text=True)
refused("shipped installer refuses with an empty trust list", proc.returncode, proc.stderr,
        "no trusted release key provisioned")
check("empty-trust refusal happens before any fetch", "cannot fetch" not in proc.stderr,
      proc.stderr.strip()[:300])

# ------------------------------------------------------------ [V] usage and validation refusals
for label, args, needle, want in [
    ("bad version", ["--version", "0.1"], "invalid version", 1),
    ("empty prefix", ["--version", VERSION, "--prefix", ""], "--prefix must not be empty", 1),
    ("unknown target", ["--version", VERSION, "--target", "x86_64-apple-darwin",
                        "--prefix", os.path.join(WORK, "p")], "unsupported target", 1),
    ("unknown option", ["--version", VERSION, "--nope"], "usage:", 2),
    ("no version", [], "usage:", 2),
]:
    proc = subprocess.run(["sh", INSTALLER] + args, capture_output=True, text=True)
    check(f"usage refusal: {label}", proc.returncode == want and needle in proc.stderr,
          f"rc={proc.returncode} want rc={want} and {needle!r} in {proc.stderr.strip()[:200]!r}")

httpd.shutdown()
print(f"OKS={OKS} FAILS={FAILS}")
sys.exit(min(FAILS, 120))
