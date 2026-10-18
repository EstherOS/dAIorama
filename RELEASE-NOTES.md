# dAIorama — release notes

## 0.1.0

First release line. Everything below ships from the same signed manifest; the installer is
`packaging/release/install.sh`, the in-binary verifier is `daiorama release verify`.

### Compatibility

| Surface | This release | Compatible range | Notes |
|---|---|---|---|
| Core binary (`daiorama`) | `0.1.0` | `0.1.x` | CLI, manager and MCP bridge in one binary. Binary semver is independent of the protocol version. |
| RPC protocol | `0.1.0` | `0.1.x` | Handshake overlap rule: allowed iff `max(client.min_supported, manager.min_supported) <= min(client.protocol_version, manager.protocol_version)`. Otherwise `version_incompatible`, carrying both pairs. |
| Dashboard module | `0.1.0` | `0.1.x` | Separate artifact. It never rejects a manager on version; it degrades per manager via capability detection. |
| Release manifest | `format: 1` | `1` | The verifier refuses any other format before reading rows. |
| MCP tool names | additive-only | — | Names may be added; existing names do not change meaning. Agents may cache the surface. |

Within `0.1.x` the RPC surface, error codes and tool names are **additive-only**: new fields and
calls may appear; existing ones keep their shape and meaning. A change that breaks that rule
earns `0.2.0`, not a patch release.

### What is in 0.1.0

- **Manager + CLI + bridge** in one multiplatform binary, with the local unix socket and the
  opt-in TCP surface (instance token, per-scope secrets, per-request scope gate).
- **Sessions and views**: lifecycle, headless compositor sessions, the view front with its own
  credential and WebSocket↔RFB relay, capture, and the in-binary input tools.
- **History and retention**: per-session JSONL logs with rotation, the durable `.ended` marker,
  and the configurable retention sweep.
- **MCP bridge**: stdio and `--listen`, media pass-through, and the transport bounds (per-read
  progress, total head window, connection cap, refusal drain).
- **Dashboard module**: the module artifact, `dashboard install` (with `--version` to select an
  installed version) and `dashboard serve`, plus the UI. `dashboard serve` is loopback-only in
  v1; use SSH forwarding.
- **Packaging**: the systemd unit and runbook, the noVNC installer, the core and desktop
  container artifacts, and the release line — manifest generator (file and image digest rows),
  in-process verifier, distribution-`minisign` verifier, and the installer.

### Install

1. Verify the manifest and signature against the trust anchor in
   `packaging/release/trusted-keys` (`daiorama release verify` or `packaging/release/verify.sh`).
2. Install the verified artifact with `packaging/release/install.sh` (row name **and** target
   checked; metadata capped at 1 MiB, artifact at 128 MiB).
3. Install the dashboard module with `daiorama dashboard install` when the UI is wanted.

**Trust anchor:** `0.1.0` ships the trust list **empty**. Until the operator's public key is
added, release verification refuses with `no trusted release key provisioned` — by design, not
a defect. The keypair is generated offline; rotation is a data change to the list.

### Known limits in 0.1.0

- The reference-hardware evidence is not part of this tree: the pixel oracle at two resolutions
  and scale 2, the four live browser legs, and a compositor-rendered session inside the desktop
  image were exercised where a render node and podman socket exist, not on the build bench.
- CI and release-automation lanes are operator-side; the local gates are `cargo fmt`, `clippy`
  and the workspace test suite.
- The NVIDIA/CDI lane and the llvmpipe CI lane are operator decisions, not code paths that
  ship disabled.
