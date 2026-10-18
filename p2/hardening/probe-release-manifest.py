#!/usr/bin/env python3
"""C2 manifest live leg: a real minisign keypair against the shipped generator and verifier.

The unit tests pin `release.rs` against fixture bytes. This probe drives the shipped binary
the way a release operator would: generate a manifest over real files, sign it with a real
minisign key, and verify with builds whose trust list is the checked-in file (not a fixture).

Three binaries are needed because the trust list is `include_str!`-embedded at build time:

  <plain>   the shipped build -- trust list empty, the state every release starts in;
  <key1>    the same source with one public key appended to packaging/release/trusted-keys;
  <key12>   the same source with a second key appended as well (the rotation window).

Assertions:
  [G] the generator's bytes: valid JSON, one artifact object per line, real sha256/size,
      facts/components derived from the artifact kinds;
  [P] a POSIX-only reader (grep/sed, no jq) can pull name+sha256 out of a row -- the
      install-script contract from the pre-read;
  [E] empty trust list refuses at the door, before any crypto;
  [V] a signed manifest verifies, and the printed signer id is the id inside the public key;
  [A] `--artifact` matches basename + sha256, refuses a same-name different-bytes file, and
      refuses an unknown name;
  [S] signature-before-parse: an edited manifest is a signature failure, not a schema error,
      while a re-signed bad schema is the schema error;
  [R] key rotation: a build trusting both keys accepts either signature, and an untrusted
      signer is named by id rather than reported as a generic failure.

Usage: probe-release-manifest.py <plain> <key1> <key12> <tag>
Prints [OK]/[FAIL] per assertion plus FAILS=<n>; exit code = FAILS (capped at 120).
"""
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys

PLAIN, KEY1, KEY12, TAG = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
KEY1_PUB, KEY1_SEC, KEY2_PUB, KEY2_SEC = sys.argv[5], sys.argv[6], sys.argv[7], sys.argv[8]
WORK = "/tmp/c2manifest"
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


def run(args, timeout=120):
    proc = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
    return proc.returncode, proc.stdout, proc.stderr


def ok(label, cond, detail=""):
    report(bool(cond), label, detail)


def rc_is_zero(label, rc, out, err):
    ok(label, rc == 0, f"rc={rc} out={out.strip()[:200]} err={err.strip()[:200]}")


def refused(label, rc, err, needle):
    ok(label, rc != 0 and needle in err, f"rc={rc} want {needle!r} in {err.strip()[:300]!r}")


def digest(path):
    with open(path, "rb") as handle:
        return hashlib.sha256(handle.read()).hexdigest()


def make(path, size):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as handle:
        handle.write(bytes((index * 37 + 11) % 256 for index in range(size)))
    return path


def read_key(public):
    """Key id as printed inside a minisign `.pub` untrusted-comment line."""
    for line in open(public):
        match = re.search(r"minisign public key ([0-9A-Fa-f]{16})", line)
        if match:
            return match.group(1).upper()
    print(f"[FAIL] no key id in {public}")
    sys.exit(120)


def sign(secret, manifest, signature):
    rc, out, err = run(["minisign", "-S", "-s", secret, "-m", manifest, "-x", signature])
    if rc != 0:
        print(f"[FAIL] minisign sign: rc={rc} {err.strip()[:300]}")
        sys.exit(120)


def verify(binary, manifest, signature, artifact=None, as_json=False):
    args = [binary, "release", "verify", "--manifest", manifest, "--signature", signature]
    if artifact:
        args += ["--artifact", artifact]
    if as_json:
        args.append("--json")
    return run(args)


def main():
    shutil.rmtree(WORK, ignore_errors=True)
    os.makedirs(WORK, exist_ok=True)
    core = make(f"{WORK}/daiorama-0.1.0-x86_64-unknown-linux-gnu.tar.gz", 4096)
    dashboard = make(f"{WORK}/daiorama-dashboard-0.1.0-x86_64-unknown-linux-gnu.tar.gz", 2048)
    novnc = make(f"{WORK}/novnc-1.7.0.tar.gz", 1024)
    manifest = f"{WORK}/manifest.json"

    print(f"== C2 manifest live leg ({TAG})")
    print(f"plain={PLAIN}")
    print(f"key1={KEY1}")
    print(f"key12={KEY12}")
    for binary in (PLAIN, KEY1, KEY12):
        rc, out, _ = run([binary, "--version"])
        print(f"version {binary}: rc={rc} {out.strip()}")

    # ---------------------------------------------------------------- [G] generator
    rc, out, err = run(
        [
            PLAIN, "release", "manifest",
            "--version", "0.1.0",
            "--protocol", "1",
            "--channel", "stable",
            "--released-at", "2026-10-18T00:00:00Z",
            "--artifact", f"core:x86_64-unknown-linux-gnu:{core}",
            "--artifact", f"dashboard:x86_64-unknown-linux-gnu:{dashboard}",
            "--asset", f"novnc-1.7.0:{novnc}",
            "--out", manifest,
        ]
    )
    rc_is_zero("[G] manifest written", rc, out, err)
    ok("[G] generator names the output path", f"manifest written: {manifest}" in out, out.strip()[:200])
    text = open(manifest).read() if os.path.exists(manifest) else ""
    try:
        parsed = json.loads(text)
    except Exception as error:  # noqa: BLE001 - a parse failure is the assertion
        parsed = None
        print(f"[FAIL] [G] manifest is valid JSON :: {error}")
    ok("[G] manifest parses as JSON", parsed is not None)
    parsed = parsed or {}
    ok("[G] format is 1", parsed.get("format") == 1, str(parsed.get("format")))
    ok("[G] version carried", parsed.get("version") == "0.1.0", str(parsed.get("version")))
    ok("[G] protocol carried", parsed.get("protocol") == "1", str(parsed.get("protocol")))
    ok("[G] channel carried", parsed.get("channel") == "stable", str(parsed.get("channel")))
    ok(
        "[G] released_at carried verbatim",
        parsed.get("released_at") == "2026-10-18T00:00:00Z",
        str(parsed.get("released_at")),
    )
    rows = parsed.get("artifacts") or []
    ok("[G] two artifact rows", len(rows) == 2, str(len(rows)))
    by_name = {row.get("name"): row for row in rows if isinstance(row, dict)}
    for path, kind in ((core, "core"), (dashboard, "dashboard")):
        name = os.path.basename(path)
        row = by_name.get(name, {})
        ok(f"[G] {name}: kind {kind}", row.get("kind") == kind, str(row.get("kind")))
        ok(
            f"[G] {name}: target",
            row.get("target") == "x86_64-unknown-linux-gnu",
            str(row.get("target")),
        )
        ok(f"[G] {name}: real sha256", row.get("sha256") == digest(path), str(row.get("sha256")))
        ok(f"[G] {name}: real size", row.get("size") == os.path.getsize(path), str(row.get("size")))
    assets = parsed.get("assets") or []
    ok("[G] one asset row", len(assets) == 1, str(len(assets)))
    if assets:
        ok("[G] asset name", assets[0].get("name") == "novnc-1.7.0", str(assets[0].get("name")))
        ok("[G] asset real sha256", assets[0].get("sha256") == digest(novnc), str(assets[0].get("sha256")))
    facts = parsed.get("facts") or {}
    ok("[G] facts.client_only false", facts.get("client_only") is False, str(facts.get("client_only")))
    ok(
        "[G] facts.components from the rows (sorted, unique)",
        facts.get("components") == ["core", "dashboard"],
        str(facts.get("components")),
    )

    # one object per line: the POSIX-installer property
    row_lines = [
        line for line in text.splitlines()
        if line.startswith('    {"name"') and '"kind"' in line
    ]
    asset_lines = [
        line for line in text.splitlines()
        if line.startswith('    {"name"') and '"kind"' not in line
    ]
    ok("[G] one artifact object per line (2 rows)", len(row_lines) == 2, str(len(row_lines)))
    ok("[G] one asset object per line (1 row)", len(asset_lines) == 1, str(len(asset_lines)))
    ok(
        "[G] each artifact row is self-contained on its line",
        all(line.rstrip().endswith((",", "}")) and '"sha256"' in line for line in row_lines),
        str(row_lines[:1]),
    )

    # ---------------------------------------------------------------- [P] POSIX reader
    def fields_of(line):
        # what an installer does with no jq: split the row on the quotes
        return dict(re.findall(r'"([^"]*)":\s*"?([^",}]*)', line))

    files = {os.path.basename(path): path for path in (core, dashboard, novnc)}
    sed_ok = 0
    for line in row_lines:
        mapping = fields_of(line)
        path = files.get(os.path.basename(mapping.get("name") or ""))
        if path and mapping.get("sha256") == digest(path):
            sed_ok += 1
    ok("[P] every artifact row yields name+sha256 through a POSIX-only split", sed_ok == 2,
       f"{sed_ok}/2 rows")
    asset_fields = fields_of(asset_lines[0]) if asset_lines else {}
    ok(
        "[P] the asset row is extractable and carries the tarball digest",
        asset_fields.get("name") == "novnc-1.7.0" and asset_fields.get("sha256") == digest(novnc),
        str(asset_fields),
    )
    pinned = '"name":"' in text
    print(
        f"[NOTE] pinned no-space literal (\"name\":\"…\") present: {pinned} — the emitted row "
        f"form is `\"name\": \"` (space after the colon); the pre-read's installer sketch "
        f"pinned the no-space form, so an installer must either be written against the emitted "
        f"bytes or the generator changes. Recorded for the installer slice, not scored here."
    )

    # duplicate names are refused before anything is written
    dup = f"{WORK}/dup/manifest.json"
    os.makedirs(os.path.dirname(dup), exist_ok=True)
    rc, out, err = run(
        [
            PLAIN, "release", "manifest", "--version", "0.1.0", "--protocol", "1",
            "--released-at", "2026-10-18T00:00:00Z",
            "--artifact", f"core:t:{core}", "--artifact", f"dashboard:t:{core}",
            "--out", dup,
        ]
    )
    refused("[G] duplicate artifact basename refused", rc, err, "appears twice")

    # ---------------------------------------------------------------- [E] empty trust list
    signature = f"{manifest}.minisig"
    rc, out, err = verify(PLAIN, manifest, signature)
    refused("[E] empty trust list refuses before touching the signature", rc, err,
            "no trusted release key provisioned")

    # ---------------------------------------------------------------- [V] real signature
    id1 = read_key(KEY1_PUB)
    id2 = read_key(KEY2_PUB)
    sec1, sec2 = KEY1_SEC, KEY2_SEC
    print(f"key1 id={id1} key2 id={id2}")
    ok("[V] two distinct keys", id1 and id2 and id1 != id2, f"{id1} vs {id2}")
    sign(sec1, manifest, signature)
    rc, out, err = verify(KEY1, manifest, signature)
    rc_is_zero("[V] trusted key + good signature verifies", rc, out, err)
    ok("[V] version and protocol printed", "manifest 0.1.0 (protocol 1)" in out, out.strip()[:200])
    printed = re.search(r"signed by ([0-9A-Fa-f]+)", out)
    ok(
        "[V] printed signer id is the id inside the public key",
        bool(printed) and bool(id1) and printed.group(1).upper() == id1.upper(),
        f"want {id1}; got {out.strip()[:200]}",
    )
    ok("[V] no signature-only row claimed", "artifact " not in out, out.strip()[:200])

    rc, out, err = verify(KEY1, manifest, signature, as_json=True)
    rc_is_zero("[V] --json verifies", rc, out, err)
    try:
        payload = json.loads(out)
    except Exception:  # noqa: BLE001
        payload = {}
    ok("[V] --json carries version/protocol/key_id",
       payload.get("version") == "0.1.0" and payload.get("protocol") == "1" and payload.get("key_id"),
       json.dumps(payload)[:200])
    ok("[V] --json omits the artifact row when none was asked", "artifact" not in payload,
       json.dumps(payload)[:200])

    # ---------------------------------------------------------------- [A] artifact matching
    rc, out, err = verify(KEY1, manifest, signature, artifact=core)
    rc_is_zero("[A] matching artifact verifies", rc, out, err)
    ok(
        "[A] row printed with kind, name and sha256",
        f"artifact core {os.path.basename(core)} sha256 {digest(core)}" in out,
        out.strip()[:300],
    )
    rc, out, err = verify(KEY1, manifest, signature, artifact=core, as_json=True)
    ok(
        "[A] --json carries the matched row",
        rc == 0 and json.loads(out or "{}").get("artifact") == os.path.basename(core),
        f"rc={rc} {out.strip()[:200]}",
    )
    same_name = make(f"{WORK}/swapped/{os.path.basename(core)}", 4096)
    with open(same_name, "ab") as handle:
        handle.write(b"one byte more")
    rc, out, err = verify(KEY1, manifest, signature, artifact=same_name)
    refused("[A] same-name different bytes refused on sha256", rc, err, "does not match the manifest row")
    unknown = make(f"{WORK}/unknown/not-in-the-manifest.tar.gz", 64)
    rc, out, err = verify(KEY1, manifest, signature, artifact=unknown)
    refused("[A] unknown artifact name refused", rc, err, "is not in the manifest")
    absent = f"{WORK}/gone/{os.path.basename(core)}"
    rc, out, err = verify(KEY1, manifest, signature, artifact=absent)
    refused("[A] a listed row whose file is absent is refused on the read", rc, err, "cannot read")

    # ---------------------------------------------------------------- [S] signature-before-parse
    edited = f"{WORK}/manifest.edited.json"
    shutil.copyfile(manifest, edited)
    with open(edited, "ab") as handle:
        handle.write(b" ")
    rc, out, err = verify(KEY1, edited, signature)
    refused("[S] edited manifest is a signature failure, not a schema error", rc, err,
            "release signature verification failed")

    reschema = f"{WORK}/manifest.format2.json"
    payload = dict(parsed)
    payload["format"] = 2
    with open(reschema, "w") as handle:
        json.dump(payload, handle, indent=1)
    sign(sec1, reschema, f"{reschema}.minisig")
    rc, out, err = verify(KEY1, reschema, f"{reschema}.minisig")
    refused("[S] re-signed unsupported format refused after the signature", rc, err,
            "unsupported release manifest format 2")

    missing = f"{WORK}/manifest.noversion.json"
    payload = dict(parsed)
    del payload["version"]
    with open(missing, "w") as handle:
        json.dump(payload, handle, indent=1)
    sign(sec1, missing, f"{missing}.minisig")
    rc, out, err = verify(KEY1, missing, f"{missing}.minisig")
    refused("[S] re-signed manifest missing a field is refused by name", rc, err,
            'release manifest is missing "version"')

    garbage = f"{WORK}/garbage.minisig"
    with open(garbage, "w") as handle:
        handle.write("not a minisign signature\n")
    rc, out, err = verify(KEY1, manifest, garbage)
    refused("[S] unparseable signature refused as invalid", rc, err, "invalid release signature")

    # ---------------------------------------------------------------- [R] rotation + untrusted
    second = f"{manifest}.key2.minisig"
    sign(sec2, manifest, second)
    rc, out, err = verify(KEY1, manifest, second)
    refused("[R] a key outside the list is named as untrusted", rc, err,
            f"release signed by an untrusted key {id2}")

    rc, out, err = verify(KEY12, manifest, second)
    rc_is_zero("[R] rotation window: second key accepted once listed", rc, out, err)
    rc, out, err = verify(KEY12, manifest, signature)
    rc_is_zero("[R] rotation window: outgoing key still accepted", rc, out, err)

    # ---------------------------------------------------------------- client-only manifest
    client_only = f"{WORK}/manifest.client.json"
    rc, out, err = run(
        [
            PLAIN, "release", "manifest", "--version", "0.2.0", "--protocol", "1",
            "--released-at", "2026-10-18T00:00:00Z", "--client-only", "--out", client_only,
        ]
    )
    rc_is_zero("[C] client-only manifest written", rc, out, err)
    sign(sec1, client_only, f"{client_only}.minisig")
    rc, out, err = verify(KEY1, client_only, f"{client_only}.minisig")
    rc_is_zero("[C] client-only manifest verifies with no artifacts", rc, out, err)
    rc, out, err = verify(KEY1, client_only, f"{client_only}.minisig", artifact=core)
    refused("[C] client-only manifest has no row to match", rc, err, "is not in the manifest")

    # ---------------------------------------------------------------- generator refusals
    rc, out, err = run([PLAIN, "release", "manifest", "--version", "", "--protocol", "1",
                        "--released-at", "2026-10-18T00:00:00Z", "--out", f"{WORK}/x.json"])
    refused("[G] empty version refused", rc, err, "are required")
    rc, out, err = run([PLAIN, "release", "manifest", "--version", "1", "--protocol", "1",
                        "--released-at", "2026-10-18T00:00:00Z",
                        "--artifact", "not-a-spec", "--out", f"{WORK}/y.json"])
    refused("[G] artifact spec without kind:target:path refused", rc, err, "is not kind:target:path")
    rc, out, err = run([PLAIN, "release", "manifest", "--version", "1", "--protocol", "1",
                        "--released-at", "2026-10-18T00:00:00Z",
                        "--artifact", f"core:t:{WORK}/does-not-exist.tar.gz", "--out", f"{WORK}/z.json"])
    refused("[G] missing artifact file refused", rc, err, "cannot read")

    print()
    print(f"FAILS={FAILS}")
    print(f"OKS={OKS}")
    print("C2-MANIFEST-DONE")
    return min(FAILS, 120)


if __name__ == "__main__":
    sys.exit(main())
