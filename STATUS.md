# dAIorama — project status

_One screen, kept current._

- **P0 — frozen** @ `4eeb585` (paper in `audits/p0/`).
- **P1 — L1 green** @ `eb35a4d` (headless chain: mutter → Hyprland → wtype/grim → wayvnc).
- **P2 — complete** (contract layer, runtime, CLI, hardening; resize/scale resolved in `p2/pre-w2/`).
- **P3 — complete** (MCP bridge stdio + `--listen`, media pass-through/store, capture, transport hardening).
- **P4 — complete** (views front, token lifecycle, rate limits, history + retention, manager TCP surface).
- **P5 — active.** Packaging/units (Esther); session runtime + dashboard UI (Blingus).
  Landed: `config scope create`; dashboard seams D1–D5, module crate, core verbs, installer, UI;
  the nine in-binary input tools; the browser launcher (`src/bin/daiorama-browser.rs`, renderer
  file from the compositor title); the noVNC installer (`packaging/novnc/fetch.sh`) with
  `doctor` visibility; the bare-host systemd unit + install runbook; **both Containerfiles** —
  core (substrate, `CMD []`, Hyprland caps stripped) and desktop (core + Chromium + the launcher,
  seed entrypoint, `CMD ["daiorama","serve"]`); three compose shapes; the Quadlet unit.
  Remaining for exit: the signed release manifest + maintained install-command table (09 C2),
  CI/release lanes, the reference-box image builds/live legs, and **the browser legs on hardware**
  (pixel oracle at two resolutions + scale 2, compositor-rendered Chromium inside the desktop
  image). The root refusal is closed: `daiorama-browser` adds `--no-sandbox` only at `geteuid()`
  zero, with the container user namespace as the boundary (09).
- **Repo target:** fork `EstherOS/dAIorama`; upstream PRs post-sign-off.
- **Last green** (`27beea6`): 391 tests, 0 failed — 262 (+6 ignored) unit · 5 browser ·
  7 config_scope · 8 daemon · 9 dashboard · 3 doctor · 27 fixtures · 7 mcp · 22 mcp_listen ·
  6 network · 22 packaging · module 6 · 2 · 5; fmt/clippy clean.
- **Next:** the release manifest + install-command table (09 C2) — the last unstarted P5
  artifact; the image builds and their live legs are reference-box work (no podman here).
- **Open:** L3 llvmpipe CI (waits on the operator's workflow token) · L2 NVIDIA/CDI (operator) ·
  a body trickled byte by byte can hold a connection slot to the deadline.
