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
  **digest**.
- **Install-command table (maintained)**: distro/release → exact command →
  version, with explicit "not packaged / out of range" rows (the Fedora
  ≥ f43 Hyprland case renders "not packaged for this distribution; see the
  release notes for a COPR/upstream build" — 04); re-checked each release.
- **One multiplatform core binary** containing CLI + manager + bridge (bridge
  is a subcommand, also standalone-invokable). Versioned as core semver. The
  protocol version is its own semver; the compat table ships with release notes.
- **Dashboard is a separate module artifact** (below) — never a compile-time
  feature flag: prebuilt downloads don't shrink with flags, a real module
  shrinks the default download.
- Client-only install mode and macOS client scope are manifest facts.

## Dashboard module (C1 — a mechanism, not a name)

`daiorama dashboard install`:

- Downloads the release asset for this target triple, **version-matched to the
  core binary by default**; `--version <v>` or `--from <path|url>` (offline)
  override.
- Verifies checksum from the manifest.
- Installs to `<data>/modules/daiorama-dashboard/<version>/`; activation is an
  atomic symlink swap; the previous version stays installed — reverting is
  re-activating it explicitly (`--version <old>`), deterministic, no mystery
  rollback state.
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

The desktop image's `CMD` is the **documented exception** to the no-default-CMD rule above; the
rule is scoped to the core image. The systemd unit in `packaging/systemd/` is the bare-host
channel, the image `CMD` is the container channel — two channels, not alternatives.

The base is `debian:sid-slim`, pinned by digest at build (trixie carries no Hyprland; sid
carries 0.56.2, inside 04's supported range). The digest is a **required build argument**
(`ARG DEBIAN_SID_DIGEST`), never a checked-in default: a default goes stale silently, and the
release manifest records the digest a build actually used (C2). The core package set follows
what the binary actually runs: `wayvnc` (`view.wayvnc_command`), `grim` and `wf-recorder`
(`capture.*` defaults), the Mesa userspace, `xwayland`, `dbus`, `seatd`, `fontconfig` +
`fonts-dejavu-core`. No terminal, no app suite, no browser in core; `wtype`/`wlrctl` are the L1
harness's stand-ins and do not ship. The binary is built from source in a builder stage and
copied into the pinned base — the image builds the binary directly, it does not download a
release artifact (channel table, above).

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
