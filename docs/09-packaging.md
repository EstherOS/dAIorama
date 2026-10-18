# 09 — Packaging, releases, and the container image

## Distribution channels (§8, scope stated)

| channel | covers | scope |
|---------|--------|-------|
| GitHub Releases + install script | primary; static binary | full (client + manager) on Linux |
| Homebrew tap (not homebrew-core) | secondary | **client-only** — macOS gets client subcommands only; manager subcommands are Linux-only (stated in README + manifest, not implied) |
| crates.io | tertiary, for Rust-literate users | never required for end users |
| container image | builds the binary directly | bypasses channels entirely |
| broader packagers (nixpkgs, AUR, apt/dnf) | none | community may do it later; not chased (01) |

Install script: `--client-only` installs CLI + bridge without manager
preflight — the harness-side artifact must exist where the harness runs (an
agent's own container), and this is its named delivery (C2).

## Release manifest (C2)

- **Signed per-release manifest**: artifacts × target triples × sha256,
  signed with **minisign** and verified against the embedded release key
  before digests are trusted. Compose snippets and install commands pin by
  **digest**. The trust anchors are data — `packaging/release/trusted-keys`,
  one public key per entry, embedded at build — and ship **empty** until the
  first operator-signed release adds one; an empty list refuses with
  `no trusted release key provisioned`, and an unknown signer is
  `release signed by an untrusted key <id>`. Verification is in-process with a
  pinned pure-Rust verifier (never a shelled-out `minisign`); signing stays
  operator-side, by hand, and private key material never enters the repo.
- **Manifest generator and verifier**: `daiorama release manifest` renders the
  unsigned manifest with one artifact/asset object per line (a POSIX installer
  can read a row without `jq`); `daiorama release verify --manifest … --signature
  … [--artifact …]` checks the signature and, when asked, a local file's name +
  sha256 before any digest is trusted. Both are cold verbs: local files only, no
  network, no writes except the generator's `--out`.
- **Second verifier (release automation):** `packaging/release/verify.sh <manifest> <signature> [artifact]` uses the distribution `minisign` and the same trust list, and is the independent check release automation runs beside the binary. It reads the list from the file tree, refuses an empty list with the same string, distinguishes an untrusted key id from a bad signature, and matches artifacts by basename + sha256. Exit 0 verified, 1 refusal, 2 usage.
- **Install script (primary channel):** `packaging/release/install.sh --version <v> [--target <triple>] [--prefix <dir>] [--client-only] [--base <url-or-dir>]`. It fetches `daiorama-release-<version>.json` and its `.minisig` from the release assets — default base `https://github.com/ygypt/dAIorama/releases/download/v<version>`; `--base` names a mirror or a local directory for an offline install — and verifies with the distribution `minisign` against the trust list **embedded in the script**, never a list fetched beside the manifest (a hostile mirror could otherwise name its own key). No manifest field is read before that check passes. Refusals, in order: `no trusted release key provisioned` before any fetch or dependency check; `release signature verification failed` / `release signed by an untrusted key <id>` (the same split the binary and `verify.sh` draw); `release manifest version <v> does not match the requested <v>`; `no artifact "daiorama-<version>-<target>" in this release` (an unknown name or target is never a "latest" fallback); `release manifest row <name> targets "<row>", not "<target>"` when the signed row's `target` differs from the requested one; `artifact "<name>" sha256 <hash> does not match the manifest row <row>` before the binary is placed. The target defaults to the host's Linux GNU triple (`x86_64` / `aarch64`; `--target` overrides) and the prefix to `~/.local/bin`. It installs `daiorama` at mode 0755 through a temp file + rename, then prints `installed daiorama <version> (protocol <protocol>) <target>`, `signed by <key id>` and `sha256 <hash>`. Unless `--client-only` it then runs `manager preflight: <prefix>/daiorama doctor` and treats the exit code as advisory — the binary is installed and the doctor output names host gaps; `--client-only` skips that run and prints `client-only install: CLI and bridge; manager subcommands need Hyprland and wayvnc (doctor names them)`. The embedded trust block is kept identical to `packaging/release/trusted-keys` and a build test fails on drift; a test also reads this paragraph so these strings cannot change quietly. `--version` must be semver and `--target` one of the four pinned triples before either reaches a path or URL, and manifest/signature reads are capped at 1 MiB while the artifact is bounded by the installer's 128 MiB ceiling (metadata and binary are different file classes). Exit 0 installed, 1 refusal, 2 usage.
- **Manifest bytes (format 1)**: one JSON document plus a detached minisign
  signature. Fields: `format` (1), `version` (core semver), `protocol` (semver),
  `channel` (`stable` | `beta`), `released_at` (RFC 3339 UTC,
  `YYYY-MM-DDTHH:MM:SSZ`), `artifacts[] {name, target, kind, sha256, size}`,
  `assets[] {name, sha256, size}`, and
  `facts {client_only, manager_platforms, components}`. `kind` is `core`,
  `dashboard` or `image`; a binary row carries a Rust target triple
  (`x86_64`/`aarch64` × `gnu`/`musl`), an image row a platform (`linux/amd64`,
  `linux/arm64`) and the manifest digest in `sha256` (never a tag). At least one
  artifact row is required — a client-only release still ships the client binary.
  One artifact/asset object per line with no whitespace inside an object and a
  fixed key order, so an installer reads a row without `jq`:
  `grep -F '"name":"daiorama-<version>-<target>"'` then
  `sed -n 's/.*"sha256":"\([0-9a-f]\{64\}\)".*/\1/p'`. An image row is not a file
  row: `name` is the slug `daiorama-<version>-image-linux-amd64` (no `/`), `target`
  is the platform, `sha256` is the registry's immutable manifest digest (never a
  tag, never a hash of the image tar) and `size` is `0` because a digest carries no
  byte count; `release manifest --image <platform>:<digest>` writes it without
  reading a file. The verifier validates
  every row after the signature verifies (unknown target/kind/channel, a bad
  digest shape, a bad `released_at`, a duplicate row and a missing pinned field
  are refusals), bounds the manifest and signature at 1 MiB before reading them,
  and hashes `--artifact` through the installer's bounded path (128 MiB).
- **Install-command table (maintained)**: `packaging/install-commands.md` —
  distro/release → exact command → version, with explicit "not packaged / out
  of range" rows (the Fedora ≥ f43 Hyprland case renders "not packaged for this
  distribution; see the release notes for a COPR/upstream build" — 04), and the
  date the rows were checked. Re-checked each release. `doctor` resolves its
  in-binary rows from this table: it prints no command the table does not carry,
  and the table carries no command the binary cannot print (the reader pin fails
  the build on drift). A version outside 04's supported range is an "out of
  range" row, never an install command — Debian's Hyprland row therefore keys on
  the release codename (sid/forky carry it; stable does not), not the id alone.
- **One multiplatform core binary** containing CLI + manager + bridge (bridge
  is a subcommand, also standalone-invokable). Versioned as core semver. The
  protocol version is its own semver; the compat table ships in `RELEASE-NOTES.md`,
  one `## <version>` section per release. A build test reads the section for the
  current `CARGO_PKG_VERSION` and fails when it is missing or a row disagrees with
  the code (`Cargo.toml`, `ipc::PROTOCOL_VERSION`, the dashboard crate's version,
  `release::MANIFEST_FORMAT`), so a version bump cannot ship stale ranges. A
  supported-range cell follows the protocol's minor series; a range that spans
  series is spelled out by hand and the test's prefix is updated with it.
- **Dashboard is a separate module artifact** (below) — never a compile-time
  feature flag: prebuilt downloads don't shrink with flags, a real module
  shrinks the default download.
- Client-only install mode and macOS client scope are manifest facts.

## Dashboard module (C1 — a mechanism, not a name)

`daiorama dashboard install`:

- **Offline in v1:** the verb takes a local release asset and its digest —
  `--artifact <path.tar.gz> --sha256 <hex>`. The release tooling (and the install
  script) downloads; the manager installs only bytes it can verify. There is no
  HTTP client in the core by design, and a download the checksum does not cover
  is not an install.
- Asset: `daiorama-dashboard-<version>-<target-triple>.tar.gz`, checksummed by
  the same signed manifest as core (C2); the version is read from the asset name
  unless `--version` names the install directory explicitly.
- **The checksum gate runs first.** The artifact is hashed before any path under
  `modules/` is created, so a mismatch installs nothing and leaves the module
  tree exactly as it was.
- **The unpacked size is bounded too.** Member sizes are summed before anything
  is written: past 128 MiB unpacked, or past 4,096 entries, the install refuses
  with its pinned string and the stage is removed. The digest is still the trust
  boundary — this is the volume-fill guard behind it, since a small artifact can
  inflate far past its compressed size.
- **Stage, then one rename:** the archive unpacks into
  `<data>/modules/daiorama-dashboard/.stage-<pid>/`; members must be relative
  regular files or directories (no links, no escapes), and the module binary
  must be at the archive root. The staged tree is renamed to `<version>/`.
- Activation is one atomic symlink swap: `.active.tmp-<pid>` → `<version>` is
  renamed over `active`, so the link never names a missing directory (D1's read
  rule; the target is a single slug component).
- Replacing the active version (`--force`) clears `active` first, so the window
  reads "not installed" rather than a link dangling at a directory that is being
  renamed away; a failed install restores the version directory and the link, so
  the tree is exactly what it was.
- An existing version directory refuses unless `--force`, and `--force` replaces
  a real directory only: a symlinked (or otherwise non-directory) version path is
  never replaced — a link the operator pointed elsewhere is not an install to
  overwrite.
- The previous version stays installed — reverting is re-activating it
  explicitly (`dashboard install --version <old>`, no artifact), deterministic,
  no mystery rollback state.
- **Container case stated:** runtime downloads evaporate on container recreate.
  The module belongs on the persisted `/data` mount (08); the alternative —
  an image variant that bundles it — is documented alongside. Compose snippets
  cover both.
- `daiorama dashboard serve` refuses with the pinned 04 string when absent.
  The dashboard is never something that springs into existence as a side
  effect of `--view` or anything else.

## Container image (§9)

- **No default `CMD`** — reflected honestly, with ready-to-copy compose
  snippets, one per legitimate shape: bare bridge → remote manager; full local
  stack; manager-only exposing itself to a thin remote bridge; harness + desktop
  sharing one image. Snippets are the documentation.
- **Containment wiring ships in the snippets:** every compose snippet carries
  the working cgroupfs delegation wiring (validated in the spike) so the
  flagship shape passes the C6 check out of the box on hosts that support
  delegation; hosts that cannot delegate use the recorded `limits.mode = off`
  downgrade (06), stated in the snippet comments.
- Common-case compose stays genuinely simple: one GPU line, the MCP port, the
  **view range `6080-6099`** (publish only when views serve remote consumers,
  with `view.advertise_base` set — 05/07), optional volume; per-instance
  `view.port_base` on multi-instance hosts (08).
- **Persistence scoped narrowly:** browser profile, cookies, logins survive;
  session state is disposable by design and gets no volume.
- **Bridge binding supports both shapes equally:** published port (flagship —
  agent's container → dAIorama's container) and localhost/mounted unix socket
  (harness installed in the same image). No binding assumption that serves one.
- **Shared/central shape:** a single central instance many agents point at via
  `--remote` — zero special handling, same manager, same registry.
- **GPU recipes** (C4/C5), restated vendor-agnostically, with host
  prerequisites: **render node — any `/dev/dri/renderD*` (AMD, Intel,
  NVIDIA-on-nouveau, …)** — `--device /dev/dri/renderD128`, `--group-add
  keep-groups`, SELinux boolean where enforcing; group-mapping rough edges are
  currently active in Podman and `ls -l` on the target is the source of truth.
  **CDI** (`--device nvidia.com/gpu=all`; host requires Container Toolkit
  ≥ 1.18) is the NVIDIA-**proprietary** alternative only. `doctor`'s detection
  matrix carries the nouveau row (nouveau ⇒ render-node recipe; CDI not
  applicable). Exact recipes resolve in the spike; compose snippets carry the
  validated versions.
- Crash handling: no auto-restart of the compositor; dead session + explicit
  fresh `new` (06).

### Image split — core and desktop (P5)

Two artifacts, one Containerfile each (pre-read and rationale: `p5/image/README.md`):

| artifact | carries | `CMD` |
|----------|---------|-------|
| `daiorama` (core) | compositor stack (Hyprland + the `mutter` parent), the binary, capture/view tools, fonts/locale, D-Bus/Xwayland | none |
| `daiorama-desktop` | core + pinned Chromium + the session launcher | `["daiorama","serve"]` |

Both Containerfiles live in `packaging/container/`: `Containerfile.core` (no default command) and
`Containerfile.desktop` (`ARG CORE_IMAGE` required, launcher-only build stage, pinned Chromium,
`CMD ["daiorama","serve"]`). The desktop build consumes a core image reference; the release
manifest records the core digest it was built on. Offline builds pass `NOVNC_TARBALL=<file>` with
the pinned tarball in `packaging/novnc/`; `fetch.sh` verifies the same sha256 either way.

**Container user.** Both images run as uid 0 inside the rootless user namespace; the namespace,
not an in-container account, is the security boundary. Chromium refuses to start as root without
`--no-sandbox`, so `daiorama-browser` adds that flag only when `geteuid() == 0` (bare-host runs
never get it) and says so on stderr. Hyprland's root escape is already passed by the session
spawner. A non-root image user is a possible later shape, not this one.

The desktop image's `CMD` is the **documented exception** to the no-default-CMD rule above; the
rule is scoped to the core image. The systemd unit in `packaging/systemd/` is the bare-host
channel, the image `CMD` is the container channel — two channels, not alternatives.

The base is `debian:sid-slim`, pinned by digest at build (trixie's main archive carries no
Hyprland and trixie-backports' 0.55.2 is below 04's 0.56 range; sid carries 0.56.2, inside it).
The digest is a **required build argument**
(`ARG DEBIAN_SID_DIGEST`), never a checked-in default: a default goes stale silently, and the
release manifest records the digest a build actually used (C2). The core package set follows
what the binary actually runs: `wayvnc` (`view.wayvnc_command`), `grim` and `wf-recorder`
(`capture.*` defaults), the Mesa userspace, `xwayland`, `dbus`, `seatd`, `fontconfig` +
`fonts-dejavu-core`. No terminal, no app suite, no browser in core; `wtype`/`wlrctl` are the L1
harness's stand-ins and do not ship. The binary is built from source in a builder stage and
copied into the pinned base — the image builds the binary directly, it does not download a
release artifact (channel table, above).

The core image's "no default command" is a property of the artifact, not of the
Containerfile text: the base image ships `CMD ["bash"]`, so the build ends with an
explicit `CMD []` and `packaging/container/verify-image.sh` checks `podman inspect`
plus a real `Hyprland --version` exec. Debian's Hyprland carries `cap_sys_nice=ep`;
a rootless container cannot gain that capability, so the image strips the file
capability — the compositor runs without RT scheduling, and `--cap-add=SYS_NICE` is
not an option.

Container assets live beside the docs: `packaging/container/Containerfile.core` (the core
artifact), `packaging/compose/*.yml` (one file per shape named above) and
`packaging/quadlet/daiorama.container` (the supervised container channel). The desktop
Containerfile follows the session launcher that 06 pins at `/usr/local/bin/daiorama-browser`;
the core image is the substrate every shape composes on.

The image sets `ENV XDG_DATA_HOME=/data`, `XDG_CONFIG_HOME=/config` and `XDG_RUNTIME_DIR=/run`,
so the manager, the entrypoint and the compose mounts resolve one path set — 08's container row
(`/config`, `/data`, `/run/daiorama/<instance>/`). The desktop image's one extra requirement is
the data root: the seed must land where the front looks, so it shares that pin rather than
setting its own.

**The noVNC tree stays data.** The desktop image carries one read-only seed at
`/usr/share/daiorama/novnc`; `packaging/container/entrypoint.sh` copies it into
`<data root>/novnc` only when `vnc.html` is absent, then `exec`s the command. A recreate keeps
the volume's tree and an image bump never changes what the front serves mid-session.

## Bare-host dependencies and version checks (C5)

| dependency | check | on failure |
|------------|-------|------------|
| Hyprland | presence + version range | missing: exact install command for the detected package manager + point at the image; out of range: named-mismatch error (strings, 04) |
| wayvnc | presence + version | install command / error |
| screen capture | — decided below | — |
| browser | detected (bare-host: the user's own; container image: ships Chromium) | bare-host: refusal naming the requirement; profile persistence per 09 volumes |
| noVNC assets | the tree at `<data>/novnc` (entry point `vnc.html`) | `doctor` reports the tree and the staging command; the front refuses `view_unavailable` until it exists (07) |
| clipboard | capability check at session start | degrade loudly in `doctor`, not silently |

**View assets decision (C5, made):** the noVNC tree is **staged data, not an embedded
blob** — the front serves `<data>/novnc` (07), so the operator (or the image build) stages
the pinned tree once with `packaging/novnc/fetch.sh`; `doctor` reads the entry point the
front actually looks up and names the command when it is absent. The pin, the licence
matrix and the staging recipe are recorded in `p5/novnc/README.md`; carrying the tarball
hash in the release manifest is an open item (11).

**Screen capture decision (C5, made):** in-binary screencopy-protocol client.
No external `grim`-style tool — this keeps the static-binary claim intact.
The browser question is likewise named, not deferred: who supplies it and
where it lives is in the table above.

**Version-check pattern (§8):** startup checks Hyprland's IPC surface against
a known-compatible range and fails with a specific, named error — a confusing
downstream IPC failure becomes an immediate legible one. The gate covers the
supported **config dialect** as well as the IPC surface (a generated config is
a launch-time failure surface the IPC check alone would not catch). In-container startup
additionally verifies the actual renderer and fails loud (03); it does not
attempt to diagnose GPU passthrough from inside the container it can't see
(that is `doctor`'s job, 10).

## Testing seams = spike exit criteria (C7)

The P0 spike must **leave behind a repeatable harness**, not just a result:

- a committed container script that reproduces the full chain,
- a CI lane running protocol-level tests against the `headless` backend under
  software rendering (correctness only),
- a separate manual/GPU lane for real-hardware checks,
- fake-manager fixtures + MCP conformance fixtures,
- the renderer check.

Phrasing tension resolved: llvmpipe is functional enough for protocol tests
and **never acceptable for real sessions** (fingerprint + performance) — that
distinction is exactly why the fail-loud check exists. The real-world software-
fallback incident found during audit is the case study, not paranoia.

**Spike chain (P0, load-bearing, before anything is built on it):** rootless
Podman → render-node access → headless Hyprland → virtual pointer/keyboard →
screen capture → wayvnc → noVNC → a real, non-llvmpipe WebGL renderer string
in an actual browser. Status (2026-10-15, reference box; `p1/`): the chain
holds via the nested-compositor shape — rootless + `cap_drop ALL` + render
node only; an invisible parent → Hyprland nested → on-demand `HEADLESS-*`
outputs; keyboard (`wtype`) and pointer (`wlrctl`) injection, capture,
wayvnc → noVNC, and a real-GPU WebGL string (`ANGLE (Mesa, NV117)`) all
verified; runs 2–4 clean (`exit=0`). The nested fallback proved real, not
aspirational (niri remains untested); the resize/scale path resolved in P2 —
`p2/pre-w2/resize-scale/` (mode/scale matrix + live resize, captures verified)
and the spawner's `(logical, scale) → mode`.

The spike also proves, per 06/07: the `renderer.allow_software` override
end-to-end and the launcher capture mechanism (browser GL string without any
automation protocol); the cgroup delegation wiring in-container; the startup
sweep against a killed manager's subtree; and the view credential + media
handoff over a non-loopback bind. If the chosen build cannot run on llvmpipe
at all (upstream #3516), the CI lane changes substrate (nested compositor or
fixtures-only protocol tests) and says so.
