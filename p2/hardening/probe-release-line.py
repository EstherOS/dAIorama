#!/usr/bin/env python3
"""C2 release-line live leg at the software-closed tip: image digest rows, and the two
verifiers agreeing on the same freshly generated + signed bytes.

The earlier manifest leg (probe-release-manifest.py) exercised file rows against the binary
only; the script verifier landed while that leg was running and image rows landed after it.
This leg drives the pair an operator actually has:

  [G]  generator: file row + amd64/arm64 image rows in one manifest, compact POSIX-readable
       bytes, real digests/sizes, facts from file rows only;
  [GR] generator refusals: tag-as-digest, wrong length, uppercase hex, unknown platform,
       duplicate image slot, and `--artifact ...:image:` pointing at a file;
  [V]  binary verifier: the signed manifest verifies, the signer id is the one in the key,
       `--artifact` matches the file row, and an image row can never be satisfied by bytes;
  [S]  script verifier (`packaging/release/verify.sh` + real minisign): same bytes, same
       signer, same artifact digest, same refusals (untrusted signer vs failed signature),
       and its refusal order (empty trust list before the minisign dependency);
  [D]  divergences between the two implementations, printed as observations: the script is a
       signature/format/artifact check and does not re-validate row schema.

Usage: probe-release-line.py <key1-binary> <key1.pub> <key1.sec> <key2.pub> <key2.sec>
                             <verify.sh> <tag>
Prints [OK]/[FAIL] per assertion plus FAILS=<n>; exit code = FAILS (capped at 120).
"""
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile

KEY1_BIN, KEY1_PUB, KEY1_SEC, KEY2_PUB, KEY2_SEC, SCRIPT, TAG = sys.argv[1:8]
WORK = "/tmp/c2line"
FAILS = 0
OKS = 0
LAYOUTS = 0


def report(ok, label, detail=""):
    global FAILS, OKS
    if ok:
        OKS += 1
        print(f"[OK]   {label}")
    else:
        FAILS += 1
        print(f"[FAIL] {label}" + (f" :: {detail}" if detail else ""))


def ok(label, cond, detail=""):
    report(bool(cond), label, detail)


def run(args, timeout=180, env=None):
    proc = subprocess.run(args, capture_output=True, text=True, timeout=timeout, env=env)
    return proc.returncode, proc.stdout, proc.stderr


def rc_is_zero(label, rc, out, err):
    ok(label, rc == 0, f"rc={rc} out={out.strip()[:200]} err={err.strip()[:300]}")


def refused(label, rc, err, needle):
    ok(label, rc != 0 and needle in err, f"rc={rc} want {needle!r} in {err.strip()[:300]!r}")


def digest(path):
    with open(path, "rb") as handle:
        return hashlib.sha256(handle.read()).hexdigest()


def make(path, size, seed=0):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as handle:
        handle.write(bytes((index * 37 + 11 + seed) % 256 for index in range(size)))
    return path


def read_key(public):
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


def script_layout(trusted_keys):
    """A release-automation install layout: verify.sh finds trusted-keys beside itself."""
    global LAYOUTS
    LAYOUTS += 1
    path = f"{WORK}/layout{LAYOUTS}"
    os.makedirs(path, exist_ok=True)
    shutil.copyfile(SCRIPT, f"{path}/verify.sh")
    os.chmod(f"{path}/verify.sh", 0o755)
    with open(f"{path}/trusted-keys", "w") as handle:
        handle.write(trusted_keys)
    return f"{path}/verify.sh"


def script_run(trusted_keys, manifest, signature=None, artifact=None, path_env=None):
    script = script_layout(trusted_keys)
    # Absolute interpreter: PATH is being narrowed on purpose in some cases.
    args = ["/bin/sh", script, manifest]
    if signature is not None:
        args.append(signature)
    if artifact:
        args.append(artifact)
    env = os.environ.copy()
    if path_env is not None:
        env["PATH"] = path_env
    return run(args, env=env)


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
    manifest = f"{WORK}/manifest.json"
    external = f"{WORK}/make-0.1.0-x86_64-unknown-linux-gnu.tar.gz"
    make(external, 640, seed=5)

    digest_amd64 = hashlib.sha256(b"registry manifest digest amd64").hexdigest()
    digest_arm64 = hashlib.sha256(b"registry manifest digest arm64").hexdigest()
    id1 = read_key(KEY1_PUB)
    id2 = read_key(KEY2_PUB)

    print(f"== C2 release-line live leg ({TAG})")
    print(f"binary={KEY1_BIN}")
    print(f"script={SCRIPT}")
    print(f"key1={id1} key2={id2}")
    rc, out, _ = run([KEY1_BIN, "--version"])
    print(f"version: rc={rc} {out.strip()}")

    # ------------------------------------------------------------------ [G] generator
    rc, out, err = run(
        [
            KEY1_BIN, "release", "manifest",
            "--version", "0.1.0",
            "--protocol", "0.1.0",
            "--channel", "stable",
            "--released-at", "2026-10-18T08:00:00Z",
            "--artifact", f"core:x86_64-unknown-linux-gnu:{core}",
            "--image", f"linux/amd64:{digest_amd64}",
            "--image", f"linux/arm64:{digest_arm64}",
            "--out", manifest,
        ]
    )
    rc_is_zero("[G] manifest with a file row and two image rows written", rc, out, err)
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
    ok("[G] protocol carried verbatim", parsed.get("protocol") == "0.1.0", str(parsed.get("protocol")))
    rows = parsed.get("artifacts") or []
    ok("[G] three artifact rows", len(rows) == 3, str(len(rows)))
    by_name = {row.get("name"): row for row in rows if isinstance(row, dict)}
    core_name = os.path.basename(core)
    core_row = by_name.get(core_name, {})
    ok("[G] file row: kind core", core_row.get("kind") == "core", str(core_row.get("kind")))
    ok("[G] file row: target triple", core_row.get("target") == "x86_64-unknown-linux-gnu",
       str(core_row.get("target")))
    ok("[G] file row: real sha256", core_row.get("sha256") == digest(core), str(core_row.get("sha256")))
    ok("[G] file row: real size", core_row.get("size") == os.path.getsize(core), str(core_row.get("size")))
    for platform, expected in (("linux/amd64", digest_amd64), ("linux/arm64", digest_arm64)):
        slug = f"daiorama-0.1.0-image-{platform.replace('/', '-')}"
        row = by_name.get(slug, {})
        ok(f"[G] image {platform}: pinned slug name", bool(row),
           f"names={sorted(str(name) for name in by_name)}")
        ok(f"[G] image {platform}: target is the platform", row.get("target") == platform,
           str(row.get("target")))
        ok(f"[G] image {platform}: kind image", row.get("kind") == "image", str(row.get("kind")))
        ok(f"[G] image {platform}: registry digest verbatim", row.get("sha256") == expected,
           str(row.get("sha256")))
        ok(f"[G] image {platform}: size 0 sentinel", row.get("size") == 0, str(row.get("size")))
    facts = parsed.get("facts") or {}
    ok("[G] facts.components lists file kinds only", facts.get("components") == ["core"],
       str(facts.get("components")))
    ok("[G] facts.client_only false", facts.get("client_only") is False, str(facts.get("client_only")))

    # Compact bytes: the pinned POSIX extractor form is `"name":"…"` with no space.
    ok("[G] rows are compact (no space after the colon)", '"name":"' in text and '"name": "' not in text,
       text.splitlines()[6][:120] if len(text.splitlines()) > 6 else text[:120])
    slug_amd64 = "daiorama-0.1.0-image-linux-amd64"
    grep = subprocess.run(["grep", "-F", f'"name":"{slug_amd64}"', manifest],
                          capture_output=True, text=True)
    ok("[G] POSIX grep -F finds the image row by pinned name", grep.returncode == 0, grep.stdout[:200])
    lines = [line for line in text.splitlines() if f'"name":"{slug_amd64}"' in line]
    extracted = ""
    if lines:
        match = re.search(r'"sha256":"([0-9a-f]{64})"', lines[0])
        extracted = match.group(1) if match else ""
    ok("[G] POSIX sed extracts the image digest from that row", extracted == digest_amd64, extracted[:80])
    ok("[G] every artifact row is one object on one line",
       all(any(f'"name":"{row["name"]}"' in line for line in text.splitlines()) for row in rows),
       "rows not all on single lines")

    # ------------------------------------------------------------------ [GR] refusals
    base = [KEY1_BIN, "release", "manifest", "--version", "0.1.0", "--protocol", "0.1.0",
            "--released-at", "2026-10-18T08:00:00Z"]
    rc, out, err = run(base + ["--image", "linux/amd64:latest", "--out", f"{WORK}/g1.json"])
    refused("[GR] a tag where a digest belongs is refused", rc, err,
            "expected 64 lowercase hex characters")
    rc, out, err = run(base + ["--image", f"linux/amd64:{digest_amd64[:-1]}", "--out", f"{WORK}/g2.json"])
    refused("[GR] a 63-character digest is refused", rc, err, "expected 64 lowercase hex characters")
    rc, out, err = run(base + ["--image", f"linux/amd64:{digest_amd64.upper()}", "--out", f"{WORK}/g3.json"])
    refused("[GR] an uppercase digest is refused", rc, err, "expected 64 lowercase hex characters")
    rc, out, err = run(base + ["--image", f"linux/386:{digest_amd64}", "--out", f"{WORK}/g4.json"])
    refused("[GR] an unbuilt platform is refused", rc, err, 'outside the release set for kind "image"')
    rc, out, err = run(base + ["--image", f"linux/amd64:{digest_amd64}",
                               "--image", f"linux/amd64:{digest_arm64}", "--out", f"{WORK}/g5.json"])
    refused("[GR] the same platform twice is a duplicate row", rc, err, "duplicate release manifest row")
    rc, out, err = run(base + ["--artifact", f"image:linux/amd64:{core}", "--out", f"{WORK}/g6.json"])
    refused("[GR] an image row cannot be built from a file", rc, err,
            "image rows are digest rows; pass them with --image")
    rc, out, err = run([KEY1_BIN, "release", "manifest", "--version", "0.1.0", "--protocol", "0.1.0",
                        "--released-at", "2026-10-18T08:00:00Z", "--out", f"{WORK}/g7.json"])
    refused("[GR] an empty manifest is refused", rc, err, "lists no artifacts")

    # ------------------------------------------------------------------ [V] binary verify
    signature = f"{manifest}.minisig"
    sign(KEY1_SEC, manifest, signature)
    rc, out, err = verify(KEY1_BIN, manifest, signature)
    rc_is_zero("[V] signed image-carrying manifest verifies", rc, out, err)
    ok("[V] version and protocol printed", "manifest 0.1.0 (protocol 0.1.0)" in out, out.strip()[:200])
    printed = re.search(r"signed by ([0-9A-Fa-f]+)", out)
    ok("[V] printed signer id is the id inside the public key",
       bool(printed) and printed.group(1).upper() == id1, f"want {id1}; got {out.strip()[:200]}")
    rc, out, err = verify(KEY1_BIN, manifest, signature, as_json=True)
    try:
        payload = json.loads(out or "{}")
    except Exception:  # noqa: BLE001
        payload = {}
    ok("[V] --json carries version/protocol/key_id",
       rc == 0 and payload.get("version") == "0.1.0" and payload.get("protocol") == "0.1.0"
       and payload.get("key_id"), f"rc={rc} {out.strip()[:200]}")
    rc, out, err = verify(KEY1_BIN, manifest, signature, artifact=core)
    rc_is_zero("[V] the file row verifies with --artifact", rc, out, err)
    ok("[V] row printed with kind, name and sha256",
       f"artifact core {core_name} sha256 {digest(core)}" in out, out.strip()[:300])
    impostor = make(f"{WORK}/impostor/{slug_amd64}", 100)
    rc, out, err = verify(KEY1_BIN, manifest, signature, artifact=impostor)
    refused("[V] a file named like an image row cannot satisfy it", rc, err,
            "does not match the manifest row")
    rc, out, err = verify(KEY1_BIN, manifest, signature, artifact=f"{WORK}/gone/{slug_amd64}")
    refused("[V] an image row with no file behind it is refused on the read", rc, err, "cannot read")
    tampered = make(f"{WORK}/tampered/{core_name}", 4096, seed=9)
    rc, out, err = verify(KEY1_BIN, manifest, signature, artifact=tampered)
    refused("[V] same-name different bytes refused on sha256", rc, err, "does not match the manifest row")

    # ------------------------------------------------------------------ [S] script verify
    key1_trust = open(KEY1_PUB).read()
    rc, out, err = script_run(key1_trust, manifest, signature)
    rc_is_zero("[S] script verifies the same generated bytes", rc, out, err)
    ok("[S] script prints version and protocol", "manifest 0.1.0 (protocol 0.1.0)" in out, out.strip()[:200])
    printed_script = re.search(r"signed by ([0-9A-Fa-f]+)", out)
    ok("[S] script names the same signer as the binary",
       bool(printed_script) and printed_script.group(1).upper() == id1, out.strip()[:200])
    rc, out, err = script_run(key1_trust, manifest, signature, artifact=core)
    rc_is_zero("[S] script verifies the file row with the artifact argument", rc, out, err)
    ok("[S] script prints kind, name and sha256",
       f"artifact core {core_name} sha256 {digest(core)}" in out, out.strip()[:300])
    rc, out, err = script_run(key1_trust, manifest, signature, artifact=tampered)
    refused("[S] script refuses tampered bytes on the digest", rc, err, "does not match the manifest row")
    unknown_file = make(f"{WORK}/unknown/not-in-the-manifest.tar.gz", 64)
    rc, out, err = script_run(key1_trust, manifest, signature, artifact=unknown_file)
    refused("[S] script refuses an artifact name that is not a row", rc, err, "is not in the manifest")
    second = f"{manifest}.key2.minisig"
    sign(KEY2_SEC, manifest, second)
    rc, out, err = script_run(key1_trust, manifest, second)
    refused("[S] script names an untrusted signer by id", rc, err,
            f"release signed by an untrusted key {id2}")
    broken = f"{WORK}/broken.minisig"
    real = open(signature).read().splitlines()
    encoded = list(real[1])
    # Flip one base64 character well past the 8-byte key id (chars 3..14) so the id in the
    # signature still names key1 while the signature bytes no longer verify.
    flip_at = max(20, len(encoded) // 2)
    encoded[flip_at] = "A" if encoded[flip_at] != "A" else "B"
    with open(broken, "w") as handle:
        handle.write(real[0] + "\n" + "".join(encoded) + "\n")
    rc, out, err = script_run(key1_trust, manifest, broken)
    refused("[S] script reports a trusted-key bad signature as a verification failure", rc, err,
            "release signature verification failed")
    # A PATH with the script's shell tools but no minisign: refusal order is observable here.
    minbin = f"{WORK}/minbin"
    os.makedirs(minbin, exist_ok=True)
    for tool in ("dirname", "sed", "awk", "grep", "head", "mktemp", "rm", "cat", "sha256sum",
                 "base64", "od"):
        candidate = shutil.which(tool)
        if candidate and not os.path.exists(f"{minbin}/{tool}"):
            os.symlink(candidate, f"{minbin}/{tool}")
    rc, out, err = script_run("# empty trust list\n", manifest, signature, path_env=minbin)
    refused("[S] empty trust list refuses before the minisign dependency is checked", rc, err,
            "no trusted release key provisioned")
    rc, out, err = script_run(key1_trust, manifest, signature, path_env=minbin)
    refused("[S] a provisioned list without minisign names the missing dependency", rc, err,
            "minisign is required")
    rc, out, err = script_run(key1_trust, manifest)
    ok("[S] a one-argument call is a usage error (exit 2)", rc == 2, f"rc={rc} err={err.strip()[:120]}")
    rc, out, err = script_run(key1_trust, f"{WORK}/absent.json", signature)
    refused("[S] an unreadable manifest is refused", rc, err, "cannot read")

    # ------------------------------------------------------------------ [D] divergences
    print()
    print("[NOTE] divergence probe (observations, not scored):")
    lax = f"{WORK}/manifest.file-size-zero.json"
    payload = json.loads(text)
    for row in payload["artifacts"]:
        if row["kind"] == "core":
            row["size"] = 0
    with open(lax, "w") as handle:
        json.dump(payload, handle, indent=1)
    sign(KEY1_SEC, lax, f"{lax}.minisig")
    rc, out, err = verify(KEY1_BIN, lax, f"{lax}.minisig")
    print(f"  binary on a file row with size 0: rc={rc} err={err.strip()[:160]!r}")
    ok("[D] binary refuses a file row with the image size sentinel after the signature",
       rc != 0 and 'field "size" has invalid value' in err, f"rc={rc} {err.strip()[:200]}")
    rc, out, err = script_run(key1_trust, lax, f"{lax}.minisig")
    print(f"  script on the same bytes: rc={rc} out={out.strip()[:120]!r}"
          f" err={err.strip()[:120]!r}")
    ok("[D] the script is a signature/format/artifact check: it does not re-validate row schema",
       rc == 0, f"rc={rc} — if this ever fails, the two contracts converged; update 09")
    print("  image row bytes cannot be satisfied by a file in either implementation: the")
    print("  registry digest is not a hash of any local artifact, so name matching alone")
    print("  cannot pass. That is the intended `size: 0`/`kind: image` semantic.")

    print()
    print(f"FAILS={FAILS}")
    print(f"OKS={OKS}")
    print("C2-RELEASE-LINE-DONE")
    return min(FAILS, 120)


if __name__ == "__main__":
    sys.exit(main())
