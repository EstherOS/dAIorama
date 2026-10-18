# Release manifest — pre-read (P5, C2)

Status: **unbuilt.** 09's C2 decision adopts a signed per-release manifest (artifacts × target
triples × sha256, minisign, verified against an embedded release key before digests are trusted)
and names "GitHub Releases + install script" as the primary channel. Nothing in the tree emits,
verifies or consumes one: `packaging/README.md` still says *"Until the first signed release
(09, C2), build from the checkout"*, and `dashboard install` takes its `--sha256` from the
caller (C1), so the module trust chain starts at the operator. This note pins the artifact set,
the manifest's bytes, the verify recipe (measured), the key-custody boundary and the acceptance
legs, so the implementation is a code step rather than another design argument.

## What exists today

- `packaging/` ships the units, the two Containerfiles, three compose shapes and
  `install-commands.md` (C2's other half — `doctor` is bound to that table, #104).
- No generator, no verifier, no install script, no `.github/workflows`.
- `src/install.rs` unpacks a dashboard asset after its digest is checked; the digest is an
  argument, not a trust decision.
- Deps are exact-pinned; `serde_json` is present. No signature crate.

## The verify recipe (measured, not assumed)

`minisign` 0.12 from the distribution, measured in the trixie container (`libsodium23` is the only
added dependency); bookworm ships 0.11-1, Fedora 0.12, EPEL 10 0.12, and Homebrew carries the
formula — those four from the distribution indexes, not measured here. Full transcript:
`p5/release-manifest/evidence/minisign.txt`.

    minisign -G -p rel.pub -s rel.sec -W                       # keygen, non-interactive
    minisign -S -s rel.sec -m manifest.json -x manifest.json.minisig \
             -t "dAIorama release <version>"
    minisign -V -m manifest.json -x manifest.json.minisig -p rel.pub      # rc 0

The public-key file is two lines — an `untrusted comment:` line carrying the key id, then the
base64 key. The base64 line alone is enough for a verifier (`-P <base64>`), which is what an
embedded constant would be:

    untrusted comment: minisign public key 3BDCCD7F8D986451
    RWRRZJiNf83cO5oqjoWZ201NFgX9E8o8mBE9vE5gQKUriMW9GpCn8/J9

Refusals measured on the same bytes:

| what changed | rc | message |
|---|---|---|
| one digit of one digest | 1 | `Signature verification failed` |
| verified with another key | 1 | `Signature key id in … is <a>` / `but the key id in the public key is <b>` |
| signature file absent | 2 | `<path>: No such file or directory` |
| signature line corrupted | 2 | `Unsupported signature algorithm` |
| trusted comment edited | 1 | `Comment signature verification failed` |
| file renamed (bytes untouched) | **0** | verifies |

The last row is the one that shapes the format: the signature covers **bytes, not identity**. A
renamed copy of a valid manifest verifies, so `version`, `channel` and `protocol` must live
*inside* the signed bytes — the file name is a convention, never a fact. The generator must also
be deterministic: signing happens over the exact bytes that ship, so nothing may reformat the
file afterwards (no "prettier" pass in CI, no re-serialise on copy).

## The manifest

Two files per release: `daiorama-release-<version>.json` and its detached
`daiorama-release-<version>.json.minisig`.

JSON, not RON: a shell installer, CI and Rust all read it, and `serde_json` is already pinned.
Pinned fields:

    format        manifest schema version (1)
    version       core semver of this release
    protocol      protocol semver (its own line, 09)
    channel       stable | beta
    released_at   RFC 3339, UTC
    artifacts[]   {name, target, kind, sha256, size}
    assets[]      {name, sha256, size}          # staged data (noVNC tarball)
    facts         {client_only, manager_platforms, components}

`kind` is `core`, `dashboard` or `image`; `target` is the Rust target triple for binaries and the
platform string for images. Images are named by **digest**, never by tag (C2 rejects unpinned
tags) — the image row carries the manifest digest in `sha256` and the tag only as `name`.

**One artifact object per line, fixed key order.** That single layout rule is what lets a POSIX
installer read the manifest with no `jq` and no `python3`:

    grep -F "\"name\":\"daiorama-$version-$target\"" manifest.json
    sed -n 's/.*"sha256":"\([0-9a-f]\{64\}\)".*/\1/p'

A generator test pins the layout (one line per object, key order, no whitespace inside an
object) and a parser test pins the two-command extraction, so the installer's assumption is a
test, not a hope.

## Trust chain and key custody

- **Private key: operator-held, offline.** It never enters the repo, CI, or a container. A
  release is signed by a human on the release box; this note does not automate that.
- **Public key: embedded twice** — as the base64 constant in `packaging/release/install.sh` and
  in the binary (`daiorama release verify`, below). Both are the same string; a test reads the
  script's constant and the binary's constant and fails if they disagree.
- **Rotation without a lock-out:** both verifiers ship a *list* of trusted key ids; a rotation
  release carries the old and the new key, the release after drops the old. minisign already
  reports the signing key id, so a manifest names its signer without an extra field.
- **What the signature buys:** a digest is trusted only after the manifest verifies. Artifacts
  are not individually signed — the manifest's digest chain binds them. `dashboard install`
  therefore never has to trust a digest it was handed by a script that did not verify.

## The install script (bootstrap order)

`packaging/release/install.sh` — POSIX shell, no downloads in the core:

1. Fetch the manifest and its `.minisig`; **verify before reading any digest** (rc 0 required,
   output not parsed for success — the exit code is the contract).
2. Select rows by `name` + `target`. An unknown name or target is a refusal, never a
   "latest" fallback: `--version` names a version, nothing auto-upgrades.
3. Download the artifact, `sha256sum`, compare with the row — mismatch refuses before the
   binary is placed or the module tree is touched.
4. Install core to `--prefix` (default `~/.local/bin`); `--client-only` installs CLI + bridge
   without the manager preflight (09), and that mode is a manifest fact (`facts.client_only`),
   not an inference from the download.
5. Print the version, the protocol version and the signer key id — the operator's receipt.

## Where the verifier lives

**Option A — decided.** The script verifies with the distribution's `minisign` — Debian
bookworm 0.11-1 / trixie 0.12, Fedora 0.12, EPEL 10, `brew install minisign` on the client-only
macOS side — documented as a prerequisite. The binary also grows `release verify`, reading local
files only (no HTTP in the core, by design), for the module path (`dashboard install` can verify
the manifest a digest came from) and as an independent cross-check.

**Implementation constraint:** the binary uses a pinned pure-Rust verifier crate
(`minisign-verify` or equivalent), never a shelled-out `minisign`. A binary that shells out to
verify its own release is a weaker trust story than one that does not, and the gate's
dual-implementation cross-check is the reason the second implementation is worth carrying. The
crate version is exact-pinned like every other dependency; the test signs one fixture and
requires both verifiers to agree, and each refusal to agree on rc where the crate exposes it.

**Grammar (04):**

    daiorama release verify --manifest <file> --signature <file> [--artifact <path>] [--json]

With `--artifact`, hash the local file and require a matching row by **name + sha256**; an
unknown target is a refusal. Without it, signature-only. Output: manifest version, signer key id,
and the matched row when asked. Exit 0/1; no network, no writes. The verb joins 04's list with
the implementation slice.

**Option B.** Installer-only verification; the core stays signature-free. Cheaper, but
`dashboard install` keeps trusting caller-supplied digests, and an air-gapped operator has no
offline verifier.

**Option C.** Ship a per-release static verifier binary. Rejected for now: a second artifact to
trust, and pinning its digest in the script is the same circularity the manifest exists to
remove.

## Open item from 11 — the noVNC tarball hash

Pinned: the manifest carries the pinned noVNC tarball as an `assets` row with its own `sha256`,
the same value `packaging/novnc/fetch.sh` verifies; the installer does **not** re-derive it from
the core row, because the core artifact does not contain the tarball (the desktop image seeds
it at build, 09). A test reads the script's pin and the manifest generator's table and fails if
they drift.

## Acceptance legs

- **L1 generator** — fixture artifacts → one manifest; every `sha256` matches `sha256sum` of the
  file; the one-object-per-line layout invariant holds for 0, 1 and N artifacts.
- **L2 verify green** — the measured `minisign -V` recipe returns 0 on a manifest signed by a
  test key, and `daiorama release verify` agrees on the same bytes.
- **L3 refusal table** — the five refusals above, each asserted on its exact rc/message, plus
  the renamed-file case asserted as *verifies* (identity is inside the bytes).
- **L4 installer** — right digest under the wrong name refused; a mismatched digest refused
  before anything is written; an unknown target refused with no fallback; a manifest whose
  signature fails is never parsed.
- **L5 facts** — `protocol`, `client_only` and `manager_platforms` present and reader-pinned in
  09 and the README, so the docs and the generator cannot drift.
- **L6 asset row** — the noVNC row equals `packaging/novnc/fetch.sh`'s pin.

## Not in this slice

No HTTP client in the core; no auto-update or channel following; no sigstore or transparency
log; no CI release lane (still the operator's workflow token); no Homebrew tap formula; no
automation of key custody or signing.

## Decisions (answered 2026-10-18)

1. **Verifier placement: Option A.** Script + `release verify`; the binary uses a pinned pure-Rust
   verifier crate, not a shelled-out `minisign`.
2. **Grammar:** `daiorama release verify --manifest <file> --signature <file> [--artifact <path>]
   [--json]`; `--artifact` matches name + sha256 and refuses an unknown target, otherwise
   signature-only; prints manifest version, signer key id and the matched row; exit 0/1, no
   network, no writes. Added to 04 when the implementation lands.
3. **Custody confirmed:** the private key stays operator-side and offline; releases are signed by
   hand. The repo ships the public key and key id; rotation accepts both public keys while each
   manifest names its signer.
