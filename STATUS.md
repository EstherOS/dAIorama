# dAIorama — project status

_One screen, kept current; the durable index for the bot-to-bot lane (threads
are read-when-on pointers)._

- **P2 in flight** — W1 contract layer + fixtures **merged**; W2 runtime (containment
  probe · spawner · health · serve · real-chain lifecycle run) **merged**; W3 CLI
  wiring live (`serve`, `daemon start/end`) **merged**; `spec/v1` @ `9a05379`.
  Next: cgroup-scope attach + parent-death + renderer gate (runtime); doctor
  formatting + config reload (contract); W5 llvmpipe lane rides its env.
- **Phase:** P0 **frozen @ `4eeb585`** (paper: `audits/p0/`; train #1–#4
  merged) · **P1 L1 green** — nested-headless chain verified on the reference
  box (`p1/`, merged @ `eb35a4d`).
- **Branches:** `spec/v1` (P0 freeze `4eeb585` · addendum `d881196` · L1
  `eb35a4d`) · `audit/p0-blingus` … `joint/p0-blingus` — all merged.
- **Repo target:** lane keeps the fork workflow (`EstherOS/dAIorama`);
  upstream `ygypt/dAIorama` accepts our PRs whenever we send them
  (operator, 2026-10-15). First upstream PR: `ygypt/dAIorama#1` (frozen
  spec + P0 trail + P1 L1 bundle) — **merged** 2026-10-15, upstream `main`
  @ `fc376c4`.
- **Last green:** P0 train → anchor `4eeb585` · **P1 L1** — chain green on
  the reference box: runs 2–4 `exit=0`; keyboard + pointer input, capture,
  wayvnc → noVNC, non-llvmpipe WebGL.
- **Next:** P2 exit items (above); L3 llvmpipe lane when its env lands; L2
  NVIDIA/CDI stays the operator's call.
- **Open (admitted into build, with deadlines):** rate-limit numbers (verify in
  build) · Hyprland version range + GPU recipes → P1 exit · renderer
  re-verification mechanism → P1 exit · resize/scale path → P1 exit ·
  headless-path resolution — resolved (03/09/11).
