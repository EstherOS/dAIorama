# 11 — Decisions ledger

Every audit item (2026-10-05, 8 blockers / 24 majors / nits) resolved to a
numbered home. Normative text lives in the numbered files; this file is the
rationale trail — decision, why, and what was rejected — so nothing gets
re-litigated and nothing is silently re-derived.

Status legend: **adopted** = draft decision taken, awaiting sign-off review ·
**open** = genuinely deferred, tracked here, not omitted · **settled** =
carried context, not in question.

## Where sign-off attention is most needed

Entries where a fork was actively resolved in the draft and the arrow could
legitimately point either way: **A4** (scope model), **A5** (token generation
rule), **B1** (reaper default), **B3** (dead-session quota reading), **B10**
(dashboard direction), **C5** (browser provider), **D5** (RON kept), and the
naming policy in 02. Everything else adopts the audit's own recommended fix.

---

## Blockers

### A1 — Contract rule contradicted by §11 · adopted · home: 04, 05
The document claimed to pin names/flags/defaults and simultaneously disclaimed
them. Now pinned: full command tree, flags, defaults, refusal strings (04);
bridge entry `daiorama mcp` (05); env var `DAIORAMA_TOKEN` exactly (04, 07);
creation flags incl. resolution/scale with defaults (04). Openness is now
limited to internal struct names, file paths, and algorithms.
*Rejected:* leaving naming to implementation-time (§11's original stance — a
builder cannot implement a contract that disclaims itself).

### A2 — Config authority · adopted · home: 08
Per-key authority table: `remote.default` and `manager.start_mode` are
client-local (read before any manager is located); everything else is
manager-side; scopes are manager-side files. `config set` is RPC only when
`--remote` is given. Remote `config edit` is explicitly refused (not
fetch-edit-push). Reload: core via `config reload`, bind/token restart-only,
scope files read per call.
*Rejected:* uniform RPC for all keys (breaks `remote.default` routing, the
contradiction the audit found); fetch-edit-push remote editing (complexity for
near-zero benefit).

### A3 — Network enumeration + view gate · adopted · home: 07
Complete surface table (manager, **bridge**, view front, dashboard, media)
with one request-time gate rule, not bind-address checks. The view-port gate
now has a named enforcer: a manager-owned static+WebSocket **front** serving
noVNC, checking a token-derived credential, proxying to loopback wayvnc.
*Rejected:* bind-address gating (fragile under proxies/publishing); direct
wayvnc exposure (no token mechanism exists there; field-confirmed).

### A4 — Scope identity · adopted · home: 07 · **review**
Authenticated over the network: per-connection scope name + secret, validated
by the manager on the same path as the instance token; network-usable scopes
must carry secrets. stdio: env at spawn (process env is the authentication).
Local unix socket: cooperative claims, documented as such — local scope auth
would be theater next to the stated posture, and the shared/central shape is
where different boxes make it real.
*Rejected:* cooperative labels everywhere (leaves quotas/manifests bypassable
on the shape that matters); per-scope secrets everywhere (local friction,
no security gain).

### A5 — Token lifecycle · adopted · home: 07 · **review**
One rule: generate on first non-loopback bind if absent, print path + value
once, write 0600; refuse only when generation is impossible. `token show` /
`token rotate` named; rotation takes effect at next startup, and the
drops-sessions consequence is stated in output (the old "no running-state to
migrate" note missed PDEATHSIG).
*Rejected:* always-refuse-without-token (the other half of the original
contradiction); live token reload (deliberately not v1 — consequence
documented instead).

### A6 — Error/denial contract · adopted · home: 05, 04
Stable `{code, message, hint}` envelope, mapped 1:1 onto MCP tool errors; codes
are a versioned, additive-only API; `session_reaped` ≠ `session_closed` ≠
`session_dead` ≠ `session_not_found`; `list_capabilities` in the always-on set;
pagination cursors; idempotency keys with a documented one-retry rule.
*Rejected:* prose errors (consumers would string-match human text — the audit's
exact prediction).

### A7 — Version handshake · adopted · home: 05, 09
Ranges-overlap comparison rule, exact; protocol semver separate from binary;
clients pre-check (managers cannot detect newer-client calls under
additive-only); tool names additive-only; machine-readable mismatch error
carries both pairs; dashboard degrades per-manager via capability detection;
compat table ships with releases.
*Rejected:* manager-side rejection policy (impossible as described and wrong
for the museum case).

### A8 — clip_video resource link · adopted · home: 05, 07
Manager-served media endpoint `/media/<id>`: deleted at first fetch or TTL
(60 s), whichever first — that is the redefined "ephemeral"; same exposure +
token rule as all surfaces; `audit.store_clips` persists the same artifact;
no second capture.
*Rejected:* inlining video (no MCP content type exists); unspecified lifetime
(the contradiction the audit found).

---

## Lifecycle & seams

### B1 — Idle reaper · adopted · home: 06 · **review**
Activity = any session-scoped RPC **or** an attached view **or** compositor
input events; watched sessions exempt from the control-call timer (separate
off-by-default view TTL); `idle-since` surfaced; reap ≠ close (cause recorded,
`session_reaped`). **Default: off**, per lean-by-default; 10/06 carry the
recommendation to enable (~1 h) on shared/central shapes.
*Rejected:* on-by-default 1 h (implicitly contradicts explicit teardown);
control-calls-only activity (the original bug: reaps watched sessions).

### B2 — View state registry · adopted · home: 06
Registry owns `{state, address, port, generation}`; port allocated at view
start from `view.port_base` (6080) upward, released on view end; per-session
lock serializes starts; generation detects stale cached addresses; explicit
kill-on-session-death; multiple viewed sessions get consecutive ports.
*Rejected:* ephemeral ports without generation (stale caches silently dead);
port at session creation (waste for un-viewed sessions).

### B3 — Hang vs crash · adopted · home: 06 · **review**
FSM: `running → degraded → dead → closed`. Health failure action: `cgroup.kill`
the tree, mark dead, never auto-restart. Dead: resources freed, **does not**
count against `max_sessions`/ceiling, stays listable, swept by the reaper (or
operator) promptly — registry slot freed at close. Cause recorded.
The audit's two clauses ("quota freed only on close" / "dead don't count") were
read as: no live resources held; no quota consumption; entry until close —
chosen so a dead session can never block new work (the zombie-lockout reading).
*Rejected:* dead counts toward quotas until closed (starvation possible while
sweep lags); auto-restart (crash-loop masking).

### B4 — PDEATHSIG scope · adopted · home: 03, 06
Stated precisely: direct children only; grandchildren covered by `cgroup.kill`
completion on crash/close paths; child re-checks `getppid()` after prctl
(fork window); non-ignorable signal configured.
*Rejected:* "clients usually exit with the compositor" as the story — not a
guarantee.

### B5 — Lazy start · adopted · home: 04, 10
Cold/hot classification table; cold commands answer "no manager running"
without starting one; `--remote` never starts anything local; deterministic
auto mode with a report line and a config pin; flock (not O_EXCL) with bounded
wait and retry-once if the winner died pre-bind.
*Rejected:* O_EXCL lockfile (stale-lock recovery burden); auto-start for cold
reads (accidental daemons).

### B6 — host backend scope + exclusivity · adopted · home: 03
"Never needs the display node" scoped to headless/nested; `host` documented as
the DRM/KMS exception (seat needed; effectively bare-metal; rootless story
unproven). Exclusivity device-global: flock on the card node + serialized
creation; loser gets `backend_busy` naming the incumbent.
*Rejected:* per-manager exclusivity (two managers race the same connector).

### B7 — Interop semantics · adopted · home: 07
Scoped to CLI/RPC discovery of foreign **managers**; identity probe (protocol
version + instance ID) before any adoption; attachment explicit, never a side
effect; foreign compositor sockets are not adoptable (ownership would be
fiction); boundary-inversion hazard documented as the guard's reason.
*Rejected:* silent adoption (well, the original "falls out for free" story).

### B8 — History & dashboard scoping · adopted · home: 06
History records carry scope; reads obey session scoping (own-scope default);
the dashboard is a named scope with an explicit, documented broad-read grant —
never default.
*Rejected:* unscoped history (leaks across differently-trusted callers);
implicit dashboard access.

### B9 — Rate limiting · **settled at the P0 reconcile** (mechanism adopted; numbers verify in build) · home: 07
Requirement on record: per-session control-call rate cap for shared/central
shapes; excess → `rate_limited`. **P0 reconcile adopted the mechanism**
(per-session token bucket over control-class calls; per-scope `limits.rate`,
`pace` default — 07); the draft numbers verify in the build. *Rejected:*
treating it as generic boilerplate; silent omission; leaving the mechanism
open past P0.

### B10 — Dashboard direction · adopted · home: 03, 10 · **review**
v1 is dashboard-initiated pull only; manager-dials-dashboard becomes an
explicit, separately specified future mechanism (config key, protocol, auth) or
an optional relay module — and the false "no special coupling" denial is
removed. v1 genuinely has zero dashboard-aware manager code because pull is
just another client.
*Rejected:* shipping dial-out in v1 (a second mechanism with an understated
N-manager token story).

---

## Packaging

### C1 — dashboard install · adopted · home: 09
Full mechanism: per-triple asset, version-matched by default, checksum from
manifest, atomic symlink activation, deterministic revert, `--from` offline
path, container case (persisted `/data` volume or image variant, documented
both).
*Rejected:* feature flag (feature flags don't shrink prebuilt downloads — the
stated rationale was wrong); runtime download without a container path
(evaporates on recreate).

### C2 — Release manifest · adopted · home: 09
Signed manifest (artifacts × triples × checksums); one multiplatform core
binary (CLI + manager + bridge; bridge-as-subcommand) + separate dashboard
module + named client-only install mode; macOS = client scope, stated; digests
pin snippets.
*Rejected:* the ambiguous "a static Rust binary" (bridge artifact ambiguity);
unpinned image tags.

### C3 — Upgrade operations · adopted · home: 10, 05
Versions in readiness; drain (reject new, report remaining); documented
sequences per shape; client-side skew display. The old "avoid while agents are
active" warning is now a procedure.
*Rejected:* warning-as-procedure.

### C4 — GPU detection · adopted · home: 10, 09
`daiorama doctor` is host-runnable and prints the exact next step per family;
in-container startup only verifies the actual renderer and fails loud.
*Rejected:* in-container diagnosis of passthrough (cannot see host state).

### C5 — Bare-host deps + capture decision · adopted · home: 09 · **review**
Dependency/version table per shape; **capture decided in-binary** (screencopy
protocol client — static-binary claim survives, no grim); **browser provider
named** (bare host: the user's; image: ships Chromium); wayvnc version check;
noVNC assets embedded; clipboard check.
*Rejected:* external capture tool (breaks static claim); unnamed browser
provider (the audit's complaint).

### C6 — cgroups v2 containment · adopted · home: 06
Preconditions stated; startup detects effective delegation, fails loud with the
exact next step, `limits.mode = off` as the recorded downgrade; bare-host
documented path (delegated systemd scope); in the spike.
*Rejected:* silent absence (containment promised, quietly not present);
containers-only scoping (bare host has a real, documentable path).

### C7 — Testing seams · adopted · home: 09
Spike leaves a committed harness: container script, CI llvmpipe protocol lane
(correctness only), separate manual/GPU lane, fake-manager + MCP conformance
fixtures, renderer check. llvmpipe policy stated: fine for protocol tests,
never for real sessions.
*Rejected:* one-time spike without a repeatable harness; letting the CI lane
imply software rendering is acceptable in production.

### C8 — Filesystem contract · adopted · home: 08
Exact path table, instance-discriminated (token/history/modules per instance);
container equivalents (`/config`, `/data`, `/run/daiorama`); referenced by
compose, backups, install.
*Rejected:* per-user paths without instance discrimination (managers collide).

---

## Ergonomics

### D1 — First run · adopted · home: 10, 04
Setup sequence spelled out; `doctor` named; `new --help` surfaces creation
options; refusal strings written out (04) instead of promised.
*Rejected:* undefined wizard contents / promised-not-written refusals.

### D2 — Start-mode visibility · adopted · home: 04, 10
`status` fields pinned (state, mode, pid, socket, versions, counts, drain);
plain `end` vs `--force` defined; one shared reachability/refusal string across
CLI, bridge, dashboard.
*Rejected:* undefined `daemon status` fields; per-surface paraphrase strings.

### D3 — Dashboard operator flows · adopted · home: 10 · (dashboard phase)
Connection CRUD; per-connection tokens in dashboard config (0600), not env
vars; the `dashboard` scope as the stated query identity; take-over with
explicit `agent|human|shared` arbitration; rename label-only; view-down vs
session-dead surfaced distinctly.
*Rejected:* env-var tokens (don't scale to N managers); undefined take-over.

### D4 — Scope ergonomics · adopted · home: 08, 04
`config scope list|get|validate`, `config effective`; startup + on-demand
validation; unknown tool/category names are load errors listing valid names
(no silent capability shrinkage); commented starter files from setup.
*Rejected:* stretching get/set over scopes (breaks the shallow rule); silent
typo shrinkage (the soggy failure the design forbids).

### D5 — Defaults + RON rationale · adopted · home: 08, 04 · **review**
History defaults pinned (8 × 4 MiB per session; 7-day post-close retention);
RON kept for config and scopes with the rationale now recorded (Rust-native,
comments, compact, serde-native); the hand-edit divergence concern is answered
by strict validation with named errors — not by changing formats. If sign-off
prefers TOML for hand-edited scope files, that is a one-file change of this
entry plus 08.
*Rejected:* TOML-for-scopes (adopted only at sign-off's preference — kept RON
pending); pinning nothing (user-visible disk behavior as placeholders).

### D6 — llvmpipe fork · adopted · home: 06, 04
One product, not two: refuse at creation when a software renderer is detected;
when unverifiable, warn + mark and re-verify at first browser launch; message
quoted in 04.
*Rejected:* warning-only path (quiet degradation is the forbidden failure).

---

## Nits (all closed)

- Theme register → policy in 02: plain/stable everywhere machine-read; theme
  nouns display-only; "daiorama"-as-session collision resolved (never normative).
- §7.1 entrypoint duplication → entrypoint pre-create dropped; the in-binary
  runtime-ensure is the single path (08). Deletion, not a second call.
- macOS tap scope → stated client-only (09, C2).
- stop-view → `stop_view`, an MCP tool in the control manifest, plus CLI `view --stop` (05, 04).
- `DAIORAMA_TOKEN` → exact name pinned (04, 07).
- Homebrew tap → kept, client-only (09).

---

## Settled context (carried — not to be relitigated)

The P0 audit's "what holds" register, kept here so nothing in it gets silently reopened:

- Manager is the only daemon; no hidden second process.
- Stateless bridge: a harness restart costs nothing; sessions belong to the manager.
- View creation, on-demand `get_view`, and `--view` are one forked-child mechanism — no duplicate build.
- The lazy-start double-start race is closed by an atomic lock at the correct seam.
- PDEATHSIG + explicitly-empty-registry-after-restart is a kernel-backed ownership story (scope caveat: B4).
- Deny-by-default manifest + the always-on session-lifecycle exception are internally coherent — and the exception is genuinely necessary for bootstrapping.
- `--remote` = one meaning, two address shapes (host:port or unix path).
- Scopes split from the dot-path config with an honest rationale; runtime-dir fix single-sourced.
- History's buffered-write/rotation reasoning (the write pattern, not the format, is the wear driver).
- wayvnc-down ≠ session-down; the startup version-check pattern; missing-Hyprland refusal prints the exact per-package-manager install command.

### Foundation — Compositor: Hyprland over niri · settled · home: 03

Chosen for today's needs: documented, stable headless/no-display operation (niri's own documentation describes its headless backend as testing-grade, with an open design question on the virtual-output API); complete virtual-input protocol support (niri's gaps are serious enough that a companion patch project exists); direct prior art for this exact use case.
*Rationale, recorded honestly:* the comparison was mixed — niri holds real non-aesthetic advantages (Rust memory safety in code sitting between the GPU and every client; a steadier release history). Hyprland wins on the two axes that matter today.
*Rejected:* niri as primary (kept as the documented fallback if Hyprland's headless path proves unworkable; the nested-compositor workaround is the other fallback). A headless X11 stack was rejected earlier for the weaker rootless input story (XTest's weak access control; ydotool needing raw kernel input access).

### Foundation — History storage writes · settled · home: 06, 08

Buffered writer on a short interval; no fsync per call; rotation + retention bound the file.
*Rationale:* the SSD-wear driver is the write *pattern*, not the format; losing seconds of buffered entries in a hard crash is acceptable because history is an operational aid, not the source of truth for session state — the registry holds that.
*Rejected:* per-call fsync; unbounded retention; structured/database logging (solves a problem the design doesn't have).

### Foundation — Headless container shape · settled · home: 03, 09

Invisible minimal parent (mutter `--headless`; `xdg_wm_base >= 6` required by aquamarine's Wayland backend — weston/sway are v5 and are rejected at bind) + nested Hyprland + on-demand `HEADLESS-*` outputs. Native standalone start is blocked with aquamarine 0.15.1 in a seatless container (allocator/seat constraints; `renderD*` is never enumerated). Evidence: `p1/` (L1, 2026-10-15). Resize/scale resolved in P2 (`p2/pre-w2/resize-scale/`).

*Rejected:* bind-clamp shimming of older parents (parked; interposition point documented in `p1/l1/xdg-clamp.c`).

## Open items roll-up (tracked, not omitted)

1. **B9 rate-limit mechanism** — settled at P0 (07); numbers verify in the build.
2. **P1 spike chain** — L1 **green** (reference box, `p1/`): nested-headless shape, input/capture/remote/WebGL verified; L2 + L3 open; resize/scale resolved in P2 (`p2/pre-w2/resize-scale/`; 09).
3. **Headless path — core resolved (P1 L1):** container shape = invisible parent + nested Hyprland + on-demand `HEADLESS-*` outputs; native standalone blocked with aquamarine 0.15.1 (allocator/seat; `renderD*` never enumerated). Evidence `p1/` (2026-10-15); resize/scale resolved in P2 (`p2/pre-w2/resize-scale/`; 03, 09).
4. **Per-family GPU recipes** — validated compose snippets, target-machine permissions as source of truth (09).
5. **cgroups delegation preconditions** — verified on real targets, in the spike (06, 09).

---

## P0 audit + reconcile (2026-10-15)

A second, independent multi-lens audit of this set (39 findings, 5 blockers —
`audits/p0/blingus-findings.md` @ `8571e3b`) was reconciled item-by-item:
dispositions in `audits/p0/reconcile-esther.md`, counter in
`audits/p0/counter-blingus.md`, joint edits applied across 00–11.

- **B9** moved open → adopted (above). **C6/C7/D6** tightened: startup
  `limits_unavailable` refusal + cold `limits.mode = off` with its true
  cleanup cost stated; the CI lane gets `renderer.allow_software` + a named
  re-verification mechanism (06). **B4** gains the startup sweep (03) and the
  fork-thread note. **B2**'s port model expands to `{front_port, vnc_port}`.
- **Sign-off shortlist additions:** F29 — no view relay in v1 (exposure model;
  flip cost = a WS relay build) · F13 — input vocabulary pinned at freeze
  (nine names; flip cost = the deferral text) · F22 — `sessions.visibility`
  moved to scope files; `--all` dropped · F30 — instance-scoped observation
  accepted and stated · F35 — rate-limit mechanism adopted (above).
- **Rejected in reconcile:** a view WS relay in v1 (deferred dashboard-phase
  mechanism, 01); kill-capable cgroups under `limits.mode = off` (keeps the
  downgrade simple; the cost is stated instead); one-time view codes
  (multi-consumer reality, 07).

---

## P2 hardening and stress findings (2026-10-16)

The build's adversarial pass (live-drive on the frozen tip; raw-socket probes) — findings that
became behavior, recorded so the readings are not re-derived.

- **Refused handshake ends the connection** — already the reading of 05 A7 (rejected at
  connect) plus the fixture's note; the pass found the manager still dispatching after a
  refusal. Fixed, regression-tested, merged (`spec/v1 @ e19e5c7`). No new pin — enforcement,
  not a new decision.

### Hardening — Read-side frame cap · adopted · home: fixtures/README

A request or response frame — the JSON text between newlines — is capped at 8 MiB
(`MAX_FRAME_BYTES`). An over-cap request frame is refused with `bad_request` (`request frame
must be at most 8388608 bytes`) and the connection ends — nothing follows the refusal, like a
refused handshake. The cap is exact: a frame at the limit is read normally.

*Rationale:* found live — the read side buffered an unbounded line, so a runaway peer could
grow the manager (the one daemon that owns every session) without limit. The cap sits orders
of magnitude above any legitimate frame (largest today: control args, config values) and far
below process-killing sizes; ending the connection is the only resync that does not cost an
unbounded drain of the oversized line.
*Rejected:* drain-and-continue (unbounded work to resync); no cap (a killable daemon).
