# dAIorama — project status

_One screen, kept current; the durable index for the bot-to-bot lane (threads
are read-when-on pointers)._

- **P2 — complete.** W1 contract layer + fixtures, W2 runtime (containment ·
  spawner · health · serve · renderer gate · real-chain run), W3 CLI, and six
  hardening rounds (lifecycle churn, drain, odd creation params, config races,
  frame/gate refusals, two-client concurrency) — **merged**; `spec/v1` @ `2d83625`.
- **P3 — W1 complete on the branch.** `p3/listen-esther`: `daiorama mcp` in
  both modes — stdio (framing, both handshakes, scope manifest, relay) and
  `--listen` (HTTP `POST /mcp`, per-request `Daiorama-Token` /
  `Daiorama-Scope` / `Daiorama-Scope-Token` gate, `Host`/`Origin` validation,
  bounded bodies, MCP-level refusals). Scope secrets pin to
  `<data>/<instance>/scope-tokens/<scope>` (0600).
- **Phase/branches:** P0 **frozen @ `4eeb585`** (paper: `audits/p0/`) · **P1 L1
  green** (`p1/` @ `eb35a4d`) · P2 through `2d83625`; P3 bridges on top
  (`p3/bridge/*`, `p3/listen` merged once cross-verified).
- **Repo target:** fork workflow (`EstherOS/dAIorama`); upstream
  `ygypt/dAIorama` takes our PRs post-sign-off.
- **Last green:** P3 W1b tree — 160 (+5 ignored) unit · 4 daemon · 2 doctor ·
  27 fixtures · 6 mcp · 9 mcp_listen, fmt/clippy clean; network live drill
  green (`p3/listen/evidence/network-live.txt`).
- **Next:** cross-verify of W1b, then W2 media/runtime calls (runtime side) and the
  capture tools on the bridge surface.
- **Open (tracked, not omitted):** L3 llvmpipe lane (waits on the operator's
  workflow token scope) · L2 NVIDIA/CDI (operator's call) · rate-limit numbers
  → P4 · per-family GPU recipes · resize/scale path · renderer re-verification
  of the L2 image · `/media/*` pass-through on the network listener (rides the
  capture tools, W2).
