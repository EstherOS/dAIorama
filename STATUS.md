# dAIorama — project status

_One screen, kept current; the durable index for the bot-to-bot lane (threads
are read-when-on pointers)._

- **P2 — workstreams complete; exit in flight.** W1 contract layer + fixtures,
  W2 runtime (containment · spawner · health · serve · renderer gate ·
  real-chain run), W3 CLI (`serve` · `daemon` · `doctor` · config reload +
  get/set · `drain`) — **all merged**; `spec/v1` @ `1997d73`. Hardening/dogfood
  rounds running (round 1: restart-after-crash refusal found; fix in progress).
- **Phase/branches:** P0 **frozen @ `4eeb585`** (paper: `audits/p0/`) · **P1 L1
  green** — nested-headless chain verified (`p1/` @ `eb35a4d`) · `spec/v1`
  carries P2 through `1997d73`; `p2/*` branches merged through #21.
- **Repo target:** fork workflow (`EstherOS/dAIorama`); upstream
  `ygypt/dAIorama` @ `fc376c4` (PR #1, merged 2026-10-15) takes our PRs when sent.
- **Last green:** tip `1997d73` — 128 (+5 ignored) unit · 4 daemon · 2 doctor ·
  27 fixtures, fmt/clippy clean; real chain lifecycle verbs green in the L1
  image; drain + config drills green (pinned strings byte-for-byte).
- **Next:** P2 exit — L3 llvmpipe lane (waits on the operator's workflow token
  scope) · L2 NVIDIA/CDI (operator's call) · hardening re-run + cross-verify.
- **Open (tracked, not omitted):** rate-limit numbers → P4 · per-family GPU
  recipes · resize/scale — resolved in P2 (`p2/pre-w2/`) · Hyprland range
  settled (`0.56.0–0.56.9`, doctor) · renderer re-verification — gate merged
  (06 D6) · headless-path — resolved (03/09/11).
