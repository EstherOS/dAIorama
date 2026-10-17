# dAIorama — project status

_One screen, kept current._

- **P0 — frozen** @ `4eeb585` (paper in `audits/p0/`).
- **P1 — L1 green** @ `eb35a4d` (headless chain: mutter → Hyprland → wtype/grim → wayvnc).
- **P2 — complete** (contract layer, runtime, CLI, hardening).
- **P3 — complete** (MCP bridge stdio + `--listen`, media pass-through/store, capture, transport hardening).
- **P4 — complete** (views front, token lifecycle, rate limits, history + retention, manager TCP surface).
- **P5 — active.** Packaging/units (Esther); dashboard module seams + noVNC sourcing (Blingus).
  Landed: pinned noVNC tree + its installer (`packaging/novnc/fetch.sh`), `doctor` reports the
  tree, dashboard seams D1–D5, `config scope create`, bare-host systemd unit + install runbook.
- **Repo target:** fork `EstherOS/dAIorama`; upstream PRs post-sign-off.
- **Last green:** 245 (+5 ignored) unit · 7 config_scope · 8 daemon · 3 doctor · 27 fixtures ·
  7 mcp · 22 mcp_listen · 6 network · 7 packaging; fmt/clippy clean.
- **Next:** container packaging (image, Quadlet unit, compose snippets).
- **Open:** L3 llvmpipe CI (waits on the operator's workflow token) · L2 NVIDIA/CDI (operator) ·
  resize/scale path · a body trickled byte by byte can hold a connection slot to the deadline.
