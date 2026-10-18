# dAIorama — project status

_One screen, kept current._

- **P0 — frozen** @ `4eeb585` (paper in `audits/p0/`).
- **P1 — L1 green** @ `eb35a4d` (headless chain: mutter → Hyprland → wtype/grim → wayvnc).
- **P2 — complete** (contract layer, runtime, CLI, hardening; resize/scale resolved in `p2/pre-w2/`).
- **P3 — complete** (MCP bridge stdio + `--listen`, media pass-through/store, capture, transport hardening).
- **P4 — complete** (views front, token lifecycle, rate limits, history + retention, manager TCP surface).
- **P5 — complete.** Packaging/units (contract); session runtime + dashboard UI (runtime).
  Landed: `config scope create`; dashboard seams D1–D5, module crate, core verbs, installer, UI;
  the nine in-binary input tools; the browser launcher (`src/bin/daiorama-browser.rs`, renderer
  file from the compositor title); the noVNC installer (`packaging/novnc/fetch.sh`) with
  `doctor` visibility; the bare-host systemd unit + install runbook; **both Containerfiles** —
  core (substrate, `CMD []`, Hyprland caps stripped) and desktop (core + Chromium + the launcher,
  seed entrypoint, `CMD ["daiorama","serve"]`); three compose shapes; the Quadlet unit;
  **the release line (C2)** — signed manifest generator and in-process verifier, the
  distribution-`minisign` verifier, image digest rows, and `packaging/release/install.sh`
  (signature-verified fetch; the trust list ships empty until the operator's first key);
  **release notes + the A7 compatibility table** (`RELEASE-NOTES.md`); the installer's name **and**
  target check, metadata/artifact caps, and semver/triple validation.
  **Hardware evidence is in:** both images rebuilt from the tip (the embedded trust list is staged
  into each build), the four browser legs (real Chromium at `ANGLE (Mesa, NV117, …)`, probe page
  gone, no-title stub, SIGTERM with no orphan), a compositor-rendered session inside the desktop
  image, and the pixel oracle — 1280×720, 1920×1080, and 1280×720 @ scale 2 → **2560×1440**, all
  `#ff0000`. The root refusal is closed: `daiorama-browser` adds `--no-sandbox` only at
  `geteuid()` zero, with the container user namespace as the boundary (09).
  Remaining for exit: the operator's release keypair and the CI/release lanes only.
- **Repo target:** fork `EstherOS/dAIorama`; upstream PRs post-sign-off.
- **Last green** (`cd85b15`): 451 tests, 0 failed — 289 (+6 ignored) unit · 5 browser ·
  7 config_scope · 8 daemon · 9 dashboard · 3 doctor · 27 fixtures · 19 install_script · 7 mcp ·
  22 mcp_listen · 6 network · 27 packaging · 6 release_manifest · 3 release_script; module
  6 · 2 · 5; fmt/clippy clean.
- **Next:** operator-side only — generate the release keypair offline and PR the public key into
  the trust anchor, then enable the CI/release lanes. Nothing on the software, documentation or
  hardware-evidence lists is open.
- **Open:** L3 llvmpipe CI (waits on the operator's workflow token) · L2 NVIDIA/CDI (operator) ·
  a body trickled byte by byte can hold a connection slot to the deadline.
