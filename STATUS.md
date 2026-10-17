# dAIorama — project status

_One screen, kept current; the durable index for the bot-to-bot lane (threads
are read-when-on pointers)._

- **P2 — complete.** W1 contract layer + fixtures, W2 runtime (containment ·
  spawner · health · serve · renderer gate · real-chain run), W3 CLI, and six
  hardening rounds (lifecycle churn, drain, odd creation params, config races,
  frame/gate refusals, two-client concurrency) — **merged**; `spec/v1` @ `2d83625`.
- **P3 — W1 in flight.** `p3/bridge-core-esther`: the stdio `daiorama mcp`
  bridge core — framing, handshake, scope manifest, `tools/call` relay,
  `list_capabilities`, and scope/origin carriage on the manager frame.
- **Phase/branches:** P0 **frozen @ `4eeb585`** (paper: `audits/p0/`) · **P1 L1
  green** (`p1/` @ `eb35a4d`) · P2 through `2d83625`; P3 bridges on top.
- **Repo target:** fork workflow (`EstherOS/dAIorama`); upstream
  `ygypt/dAIorama` takes our PRs post-sign-off.
- **Last green:** P3 W1 tree — 150 (+5 ignored) unit · 4 daemon · 2 doctor · 27
  fixtures · 5 mcp, fmt/clippy clean; bridge live drill green (`p3/bridge/`).
- **Next:** cross-verify of P3 W1, then `--listen` (network mode) and the
  capture tools on the bridge surface.
- **Open (tracked, not omitted):** L3 llvmpipe lane (waits on the operator's
  workflow token scope) · L2 NVIDIA/CDI (operator's call) · rate-limit numbers
  → P4 · per-family GPU recipes · resize/scale path · renderer re-verification
  of the L2 image.
