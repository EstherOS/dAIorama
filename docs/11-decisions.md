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
One rule: generate on the first network listen bind if absent (loopback
included; only the unix socket stays tokenless), print the path plus the
`token show` hint once — never the value (F12) — write 0600; refuse only when
generation is impossible. `token show` /
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
(media.ttl, default 600 s), whichever first — that is the redefined "ephemeral"; same exposure +
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

### Hardening — gate refusals: unanswerable lines · repeated handshakes · adopted · home: fixtures/README

A line that is not valid UTF-8 cannot be a frame: refused with `bad_request` (`request line
is not valid UTF-8`) and the connection ends. A second `handshake` on one connection is a gate
violation (05 A7: the handshake is the first call, once per connection): refused with
`bad_request` (`handshake may only be the first call on a connection`) and the connection
ends. Nothing follows either refusal, like a refused handshake.

*Rationale:* found by the third live stress pass (raw-socket battery). The non-UTF-8 line
ended the connection silently while every sibling malformed class answers — a client encoding
bug surfaced as a dead manager ("manager unreachable"), not as a refused frame. The duplicate
handshake answered the phase-1 stub envelope (`internal` + placeholder) for a call the gate
owns, not the dispatcher — the wrong class, on strings the frozen tests deliberately exclude.
*Rejected:* silence for invalid UTF-8 (no diagnostic); answering-and-continuing after a
repeated handshake (gate violations end the connection, refused handshake is the precedent).

### Hardening — dispatch refusals: unknown vs not-yet-built methods · adopted · home: fixtures/README

A method name outside the pinned surface is refused with `bad_request` (`unknown method "foo"`)
— the request names something the protocol does not define — and the connection continues
(dispatch-level, like an unknown parameter; not a gate violation). A pinned-surface method this
build does not implement yet (`get_view`, `stop_view`, `list_history`, `daemon_end`, the
control-tool names) answers `internal` (`not implemented yet`), no hint: the call is valid and
what is missing is the build. Both strings are pinned in the fixture README's frame contract.

*Rationale:* carried from the third stress pass — the default arm answered both classes with the
phase-1 stub envelope (`internal`, "not implemented yet (P2 phase 1: contract layer)", hint "the
runtime side lands in p2/session-*"): the wrong class for a typo or a newer client's call (both
read as "this manager is a skeleton", which stopped being true when P2 closed), and stale
wording for the unbuilt methods themselves. A live drill on the frozen tip showed all six name
classes answering that envelope, CLI `view`/`history`/`mcp` included.
*Rejected:* keeping the placeholder envelope for unknown calls (silence about the actual fault);
a dedicated `method_not_found` code (permanent wire surface added for a transient build state —
`bad_request` is the class that exists); listing valid names in the message (the pinned list
lives in the fixture README; the offending name is the payload); a hint on either refusal (there
is no corrective step to name).

### Hardening — config writes: one writer at a time, atomic replace · adopted · home: 08

Writers of a core-config file serialize on a cross-process `flock` (`<config>.lock`,
kernel-released on crash — the manager-lock pattern, 10) held across the whole
read-modify-write; the one entry is `config::update_config`, used by both `config set` paths
(local CLI and the manager's RPC arm). The write itself replaces the file atomically (temp
sibling + `rename`): a reader — `config get`, a reload, another process — sees the old file or
the new one, never a partial, and a crash mid-write cannot truncate it.

*Rationale:* found live by the fourth stress pass (concurrency battery, `p2/config-race/`). The
truncating in-place RMW lost updates between overlapping writers (hundreds of in-act
regressions per round; a cross-process CLI/RPC pair lost a write), tore reads mid-write (RON
parse errors on a live manager), and one run left the file persistently unparseable — an
unreadable core config blocks every later read and write, sessions included, until hand-repair.
*Rejected:* an in-process mutex only (the local CLI path races a manager across processes);
an `O_EXCL` lockfile (stale after a crash — the reason the runtime lock is flock, 10);
treating parse errors as transient and retrying (the race heals as data loss, and corruption
does not heal at all).

### Hardening — dispatch runs off the reactor · adopted · home: fixtures/README

Dispatch — capture (a configured command), `create_session` (a compositor readiness wait),
media persistence — runs on the blocking pool, never inline on a reactor worker; frames on one
connection still run strictly in order. A panic inside a dispatch answers `internal`
(`dispatch panicked`) and the connection continues.

*Rationale:* found live by the dispatch-stall battery: four `create_session` calls against a
compositor that never becomes ready occupied all four reactor workers, and a plain `status`
from a fifth connection then waited 14.5 s — the manager answered nothing at all (`status`,
`close`, `drain` included) for as long as the slow calls ran. Capture's bound is
`capture.timeout_s` (10 s default) on top of the clip's own duration (≤ 30 s), so one clip per
worker is enough to reach it in normal use, not just under attack. Both ways on the real
binary: `p2/hardening/run-dispatch-stall.sh` (14.53 s → 0.00 s).
*Rejected:* offloading only the capture methods (the next blocking dispatch reintroduces the
class); `block_in_place` (it does not add a thread — the worker still sits in the call).

## P3 bridge (2026-10-17)

### Bridge — scope and origin ride the request frame · adopted · home: 05

The manager RPC carries the caller's `scope` (absent = `operator`, the CLI path) and `origin`
(absent = `shell`; `mcp` from the bridge) as additive frame fields. The manager records them on
created sessions, reads `sessions.max` and `sessions.visibility` per call from that scope's file,
and answers `session_not_found` for a session outside the caller's visibility. The bridge loads
the same scope file for its exposed manifest (`tools/list`, `tools/call` gating) and sets both
fields on every call after its handshake.

*Rationale:* 05/07 make scope "the application-layer identity carried on every call"; without a
carriage the stdio bridge could only pretend — every MCP-created session would record `operator`
and callers would see each other's sessions. Frame fields keep params clean (the no-parameter
methods have nothing to refuse), stay additive for older managers (serde ignores unknown keys),
and leave the handshake tokenless and scope-free as 05 A7 requires.
*Rejected:* injecting `scope` into each method's params (pollutes every pinned argument surface,
and the no-params gate would refuse it); bridge-local enforcement only (the manager's own
quotas/visibility would keep answering for `operator` — a lie the bridge cannot fix); deriving
the scope from the unix socket's filesystem permissions (local trust is already cooperative, 07,
and a shared network surface needs the claim on the call itself).

### Bridge network mode — request-time gate, scope-secret files · adopted · home: 05, 07, 08

`daiorama mcp --listen <addr>` serves MCP over HTTP on one endpoint, `POST /mcp`. Every request
carries its own credentials (`Daiorama-Token`, `Daiorama-Scope`, `Daiorama-Scope-Token`) and is
gated in that order before dispatch; there is no anonymous call and no default scope. The
network bridge authenticates **per request**, not per connection: the bridge is stateless, and
HTTP connections are pooled and reused by clients, so a connection has no identity of its own —
binding one to the first request's scope would let a later request inherit the wrong identity.
Scope secrets are files, one per scope, at `<data root>/scope-tokens/<scope>` (0600, filename =
scope name): presence is what makes a scope network-usable, and a scope without one is refused
`scope_auth_required` (05/07). `config setup` mints the starter scope's secret; `--listen`
mints any missing secret for an existing scope file at startup, announced on stderr, and ensures
the instance token exists even for a loopback bind — a network surface never runs tokenless.
Gate failures answer a non-200 status with the canonical envelope and `Connection: close`
(403 for the identity gate, 400 for parse errors, 413 for a declared over-cap body); the 200
envelope is reserved for refusals that can name the request id. Never an HTTP auth challenge —
and never 401 (the OAuth-discovery trigger); bodies are bounded by `MAX_FRAME_BYTES` like a
stdio frame, and a notification answers `202` without executing.

*Rationale:* found by building W1b against 05/07: the docs named the headers and the "scope
without a secret is unusable" rule but no storage path or generation point, so every
implementation would invent one. Data-root secrets mirror the instance token (never in the
reviewable config tree, never echoed by `config scope get`); per-request auth matches the
stateless bridge and pooled HTTP. `config setup`/first-listen generation keeps "no anonymous
calls" from degrading into "no usable caller".
*Rejected:* a `secret` field inside `scopes/<scope>.ron` (config tools are readable/scriptable
and would echo it; secrets do not belong in the reviewable file); sticky per-connection scope
(the confused-deputy above); OAuth challenge/discovery metadata (harnesses without custom
headers use the stdio client-only install, 05); bind-address gating (07's request-time rule).

### Bridge network mode — a strict head, pinned transport refusals · adopted · home: 05, fixtures/README

The HTTP head parser refuses the shapes a fronting proxy refuses, because a `Content-Length`
body makes the exact byte boundaries load-bearing: more than one `Host`, or an empty `Host`
(`malformed HTTP request`); whitespace between a field name and its colon, folded continuation
lines included (`malformed HTTP request`); a repeated `Content-Length` (`duplicate
Content-Length`); and a `Content-Length` that is not `1*DIGIT` — a leading `+`, empty, any
non-digit (`invalid Content-Length: {raw}`). All are `bad_request` and the connection ends.
The four transport strings already existed in `strings.rs` but were missing from
`fixtures/README.md`; they are pinned there now, and the README-reading test covers them.

*Rationale:* the raw-HTTP adversarial pass (evidence `p3/listen/evidence/probe-http.txt`) found
four leniencies in the first parser: duplicate `Host` last-wins (the `Origin` check then reads a
different authority than a proxy would), an empty `Host` passing the "Host required" test,
`Host :` accepted after trimming the field name, and `Content-Length: +5` accepted because
Rust's `parse` takes a leading plus. Each is a disagreement about where the next request starts
— the same class as the already-refused repeated `Content-Length` and chunked forms.
*Rejected:* refusing bare-LF heads (RFC 7230 lets a recipient accept them; the MUSTs above are
the strict set); trimming a field name (that is the bug); turning any non-`POST` into 400
(405 with `Allow: POST` is the pinned shape).

## Transport limits: a per-read progress timeout and a connection cap

The bridge closes any connection whose socket makes no progress for 10 s — a head line, a body
chunk, or the idle wait for the next request on a kept-alive connection — and closes the 65th
simultaneous connection on accept. Nothing is written for either: the read side proved the peer
gone, so there is nobody to answer.

*Rationale:* the adversarial pass held 64 idle connections, a stalled head and a stalled body for
as long as it liked (`p3/listen/evidence/probe-timeout.txt`). 10 s is five times the manager
connect timeout — far above any real pause, and invisible to an idle HTTP client, which
reconnects. It is a per-read progress bound, not a total-body deadline, so an 8 MiB body stays
legal while bytes keep flowing. The cap bounds the fd set; over-cap sockets are closed on accept.

*Rejected:* a total-body deadline (60 s covers 8 MiB at ~1.1 Mbps but punishes a slow upload that
is making progress); answering a stalled request with 408 (the peer is gone).

## A head must arrive inside a total window

Once the request line has arrived, the rest of the head must arrive within 15 s; a head dribbled
one line at a time is closed with nothing written. The 10 s progress bound is per line, so a
client writing a short line every few seconds never stalls long enough to fire it and held a
connection slot until it chose to finish (`p3/listen/evidence/probe-head-deadline.txt`: answered
at 24.01 s before, closed at 15.00 s after).

*Rationale:* a head is at most 64 KiB and a real client writes it in one go; 15 s is 1.5× the
progress bound and far above any real pause. The clock starts after the request line (itself
bounded by the progress bound), so an idle kept-alive connection still closes at 10 s, and a
complete head is unaffected. The same trickle one layer down — a body written a byte at a time —
still holds a slot; it is recorded in the evidence README rather than silently fixed, because the
total-body deadline it needs was rejected above for authentic slow uploads.

*Rejected:* counting the window from accept (an idle keep-alive pause would eat it); answering
the cut with 408 (the peer is gone, like the progress bound).

## The identity gate answers before the body is read

`authenticate` runs on the head alone, before any body byte is read or parsed. An unauthenticated
caller gets `token_required` (or `scope_auth_required`) even when the body is malformed JSON: the
parser is not reachable without credentials, and its errors leak nothing.

*Rationale:* the adversarial pass (WATCH) found the body parsed before the gate — no creds plus
malformed JSON answered a parse error, so unauthenticated callers exercised the JSON parser. A
gate failure answers `Connection: close` and ends the connection: a client that sent
`Expect: 100-continue` stops on the refusal, so no phantom body is left to be read as the next
request's head, and keep-alive after a refused auth buys nothing. An over-cap body ends the
connection too.

*Rejected:* gating only `tools/call` (leaves the parser reachable through other request shapes).

## A refused request whose body is in flight keeps its refusal

Every refusal site — gate, over-cap size, malformed head, missing Host, origin, wrong method/target, chunked, duplicate/invalid `Content-Length`, and the media surface's 403/404/405/502 — writes its response, half-closes the write side, then drains
a bounded slice of the body (<= 64 KiB, <= 100 ms) before the connection closes — only when the
request carried no `Expect: 100-continue` (a client that waited never sent a body). The
half-close is what makes it reliable: the FIN goes out with the response, so the later close does
not turn into a reset that discards the refusal's body. The media surface writes its refusals as
the same `Reply` through the same `refuse` path: its first pass had a private writer that closed
without the half-close, and a body in flight ate the identity-gate body in four runs of four (evidence
`p3/media/evidence/probe-media-stall-before.txt`; with one path, four of four delivered).

*Rationale:* a client without `Expect` starts bursting the body immediately. On a 4 MiB burst
the un-drained close loses the refusal's body every time — head arrives, body arrives 0 bytes,
the client's send hits ECONNRESET (evidence `probe-lingering-close-before.txt`); with the
half-close + linger the same burst delivers the whole refusal, four runs of four (evidence
`probe-lingering-close.txt`, `probe-lingering-close.py`). 256 KiB and 1 MiB bursts are intact
either way — socket buffers absorb them.

*Rejected:* a full drain until EOF (an unauthorised client must not be able to hold a worker by
sending forever); `so_linger` (bounds the close, not the response's delivery); keep-alive after
the refusal (the earlier decision stands).

## Media pass-through on the network listener — pinned shape

`GET /media/<32 lowercase hex>` on the network listener relays the manager's media surface and
streams the answer back unchunked. The instance token is the only gate (a media fetch is not an
MCP request; the `/mcp` scope headers are not part of it), every other `/media` shape is a
bridge-side `404` + `media_expired` that never becomes an upstream request, non-GET is `405` +
`Allow: GET`, `Range` is ignored (full body), and an upstream answer without `Content-Length`
(or chunked) is `502` + `internal` rather than a guessed framing. The request target is never
logged.

*Rationale:* A8 makes the id the capability and requires pass-through with no validation; the
shape rules keep traversal and probing off the manager, and the length rule keeps the bridge
from inventing framing the manager did not send.
*Rejected:* decoding chunked upstream (the manager owns its body and lifetime); scope auth on
media fetches (the id is the credential — scope headers would imply a gate A8 does not have).

## Id-less refusals answer the HTTP status, not a 200 envelope

A request/response client matches responses by `id`. A refusal written before any body is read —
the identity gate — cannot name an id, and a 200 with `"id": null` is unmatchable: the client
waits out its read timeout and reports a dead server instead of the refusal. So the bridge's
id-less refusals answer their HTTP status with the same envelope body and `Connection: close`:
`403` for the identity gate (`token_required`, `scope_auth_required`, `scope_unknown`), `400`
for a body that does not parse, `413` for a declared body over `MAX_FRAME_BYTES`. The 200
envelope stays for refusals that can name the id (revision `-32022`, unknown method, tool
errors, the null-id request itself). `401` is never used anywhere — it is the OAuth-discovery
trigger.

*Rationale:* found by pointing the published MCP client SDK at `--listen` (p3/listen/evidence/
probe-sdk-http.txt): the gate's 200-with-null-id produced a 15 s client timeout with the
`token_required` hint discarded, while the control 403 refusal reached the client in 0 s. The
wrong/rotated token is the most common harness misconfiguration; it must not look like a dead
server. Media fetches keep the same rule (`403` + `token_required`).
*Rejected:* 401 (OAuth discovery), keep-alive after an id-less refusal (the body was never
read), making every refusal a JSON-RPC error with a synthetic id (the client would treat it as
an answer to a request it never made).

## The media surface's own bounds: one refusal close, and a write bound

Two transport properties the media surface inherited by not being written yet, found by an
adversarial pass on the merged shape (`p3/media/evidence/probe-media-stall.py`):

- **Its refusals ride the one `refuse` path.** The first pass wrote media refusals with a
  private writer that flushed and returned, so the close turned into a reset while the client
  was still bursting a body. On the pre-ruling (401-era) tree the identity-gate body arrived
  0 of 4 runs; on the before tree (`0020763`, 403 by then) the same burst is racy — the 405 1 of
  4, the 403 3 of 4. Media refusals are now `Reply`s through the same `refuse` (write →
  half-close → bounded linger unless `Expect`), and the fixed build delivers 4 of 4 for both
  statuses.
- **A client that stops reading is cut off.** Streaming wrote with `write_all` and no bound: a
  client that fetched a large artifact and accepted nothing held the task in an unbounded write
  — the read progress bound cannot fire while the task is blocked writing, so the connection
  (and its slot) stayed open for as long as the client kept quiet. Measured after 20 s of
  silence: the bridge was still streaming (31.8 MB drained, no close) and would have resumed the
  whole body. Each write now carries the same 10 s progress bound the read side does: the same
  stall closes after a bounded prefix (2.1 MB in the run) instead of waiting.
  A slow-but-progressing reader is untouched — the bound is per write, not a total-transfer
  deadline (the same posture as the body-trickle WATCH).

*Rationale:* a progress bound only means something if every path carries it; the media surface
was written after the transport fixes and quietly reintroduced both classes.
*Rejected:* a total-response deadline (kills authentic slow downloads, and the trickle WATCH
already records that a byte-at-a-time client is allowed).

## Media store — memory-only lifetime, claim-on-open, session-scoped persistence

The manager keeps media entries in memory only: `mint()` — crate-only (a private `mod media`;
`pub` inside it, with no RPC method and no CLI route — A8's "no public mint surface") — returns
a 32-hex UUID id; a fetch claims the id **on
open** (first handle wins; a truncated transfer still consumes it, and later fetches answer the
same `404 media_expired` as an expired or unknown id); `media.ttl` is read live at fetch/mint
with a lazy sweep on access; a manager restart drops every entry. `audit.store_clips` writes the
same already-captured bytes to `<data>/media/<session>/<id>` (0600, per-session 128 MiB cap,
oldest-first eviction per 08). The persisted directory keeps **08's retention window — the same
as history, seven days from session end (04)**; closing a session leaves it in place, and the
shared retention sweep removes it with the session's history. Persistence never resurrects the
URL, and never shortens history's retention.

*Rationale:* the URL is a capability (A8), not a resource id; claim-on-open removes the replay
window a complete-transfer claim leaves, and the in-process mint keeps every minting caller on
the runtime side. Session-scoped layout gives 08's per-session cap an owner and lets 08's
retention window — the one history already uses — bound the artifacts too.
*Rejected:* a persisted URL registry (a restart would resurrect capabilities); claim-on-complete
(a truncated transfer would leave the id fetchable); a manager-level media cap (08's unit is the
session).

## Capture tools — session-env commands, typed payloads, two fetchers

`screenshot` and `clip_video` run **configured argv commands** (never shell strings) under the
session's own Wayland environment, with the output path appended and nothing else
interpolated; `capture.timeout_s` bounds each call and `capture.screenshot_max_bytes` bounds
the **raw** PNG before base64 (4 MiB default: ~5.3 MiB encoded, under the 8 MiB frame
bound). The manager returns typed payloads (`capture: image | media`) and the bridge maps
them onto MCP's own content types — `image`, and a `resource_link` with the URI built at the
serving edge (`http://<Host>/media/<id>` on `--listen`; `daiorama://media/<id>` plus the
`daiorama media fetch <id> --out <path>` CLI on stdio). `store_screenshots` persists the raw
PNG URL-less (a screenshot never becomes a capability); `store_clips` rides `mint`.

*Rationale:* the command is the only compositor-dependent part, so making it configuration
keeps this slice tools-only and compositor-free in tests; the typed payload keeps the content
decision at the edge, where the transport is known. `--out` is required so stdout stays clean
for harnesses, and claim-on-open is unchanged — the CLI is just another fetcher.
*Rejected:* shell strings with parameter interpolation (an MCP parameter must never reach a
shell); inlining video (no MCP video content type — 05); a `file://` link (a path is not a
capability and leaks layout); a total-response downscale (silent loss of pixels).

## Views — in-process front, 07 credential, bounded relay

`get_view`/`stop_view` own one wayvnc per session (argv from `view.wayvnc_command`, session
Wayland env, PDEATHSIG) and one manager-owned front on its own loopback port. wayvnc 0.10 takes
no port argv, so the manager writes a per-view config (`address=127.0.0.1`, `port=<vnc_port>`)
and appends `-C <path>` — the argv pin becomes "the manager appends one generated config path",
still with no shell and no other interpolation.
The front is in-process: the strict head parser, the noVNC asset tree, and a WebSocket↔RFB relay
with explicit bounds (32 connections, 10 s per write, 64 KiB chunks) — no websockify dependency,
and no second surface on that port. The 07 credential
(`HMAC-SHA256(instance_token, "view:"||session_id||":"||generation)`) gates every request —
page, assets, upgrade — via query, `X-Daiorama-View` header, or the HttpOnly `SameSite=Strict`
cookie set only from an already-gated page load; `Origin` is required on the upgrade and must
be one of the loopback authorities for the front's port, **plus the authority of
`view.advertise_base` when set** (a browser behind a reverse proxy sends that one — the base is
display-only for the address, but its authority is a real origin); every
response carries `Referrer-Policy: no-referrer`; the credential never appears in logs, history,
stderr, or the session object (status returns the base address and generation only). A missing
asset tree is `view_unavailable`, never a partial page.

*Rationale:* the credential is one rule with one verifier, so the front must be the only thing
speaking on its port; bounds and the strict parser come from the MCP listener's lessons. An
in-process front also keeps the gate in the same process that mints the credential.
*Rejected:* shipping websockify (a second process, second version, second auth story); a
vendored minimal page (JS supply-chain and review cost for a stopgap); accepting an upgrade
without `Origin` (a browser always sends it; scripts use the header).

## Token lifecycle: the commands and their prints, and the surface that is still missing

`token show` / `token rotate` are local-instance only (refused with `--remote`), and the one
generation-time print is the path plus "run `daiorama token show`" — never the value (F12). The
network listen surface mints the token on its first bind, loopback included, because a network
listener never runs tokenless (07). `token rotate` writes the new value to the file; a running
process keeps the value it adopted at start until it restarts, and `config reload` reports
`token` under `restart_only` (the file no longer matches the adopted value).

**The manager's own TCP surface was the missing half** — `manager.bind` was a restart-only key
with no consumer in `serve`, so the "running manager keeps accepting the old token" clause had
nothing to enforce it against. `P4-ENDPOINT` (below) binds it **with** its token gate; the
rotation window is now a live-testable contract, not file semantics.

## B9 rate limit — the build

The bucket is per **(scope, session)**, continuous-refill with fractional carry, and only the
nine control-class tools spend tokens; reads, capture and lifecycle are exempt (07). Defaults
`per_sec = 20`, `burst = 40` come from the build, and `limits.rate` lives in the **scope file**
only — a core-config default would be a second source of the same numbers. `pace` parks a call
until a token exists (dispatch now runs on the blocking pool, so the wait costs a worker);
`reject` answers immediately. Both are bounded: a wait beyond `max_wait_ms` (default 30 s) or a
call arriving while `max_queued` (default 64) already wait is refused with the same
`rate_limited` envelope and a `retry after <ms> ms` hint — a flood degrades to refusals, never
to an unbounded parked queue. Capture is exempt from the bucket but **serialized**: a second
concurrent capture for a session answers `bad_request`
(`a capture is already running for this session`) — exempt must not mean free.

*Rationale:* the reject/refuse shapes reuse one envelope, so a harness branches on a code; the
queue bounds close the "time bound but unbounded length" hole a pace-only cap leaves; capture's
cost is measured in seconds, so it gets a slot, not tokens.
*Rejected:* a core-config default for the rate block (two sources); rate-limiting reads (it
punishes the observe-then-act loop); an unbounded pace queue (512 parked workers is a global
stall); counting capture against the bucket (it would throttle observation exactly when the
agent is busy acting).

## History — one JSONL line per record, rotation owned here, retention elsewhere

`<data>/history/<session>/seg-NNNN.log` holds JSONL records
`{at, event, session_id, scope, origin, cause?, detail?}` (0600 in a 0700 dir), flushed per
record to the OS and never fsynced per call (08). Rotation is this side's: 8 segments × 4 MiB,
oldest dropped. Retention is **not**: `close`/death write a durable `<session>/.ended` sidecar
(RFC 3339 + cause), and the retention sweep removes whole session directories (history and
media) seven days later — it never infers "ended when" from mtimes. `list_history` follows
**session** visibility — own scope by default, operator-wide, a stranger's id is
`session_not_found`; its cursor is opaque and session-scoped (cross-session paging is not
offered; ask per session). Session ids are opaque slugs and are validated before any path join —
a `/`, `..` or non-slug id is the same `session_not_found` as an unknown one, never a filesystem
walk.

*Rationale:* the record format and the read surface are one owner; deletion policy is another,
and a timestamp sidecar is the only interface between them. Control arguments stay out of the
log (they are the sensitive part), while counts and sizes are enough for audit and dashboard
reads.
*Rejected:* inferring session end from mtimes (fragile, and copying a directory breaks it);
cross-session cursors (a merged ordering over per-session segments needs an index the log does
not have); fsync per record (the log is an operational aid, not the source of truth).

## P4-RETENTION — the sweep that makes the window real

`history.retention` (default seven days, 04) has its consumer: the manager runs one retention
pass at startup and once an hour while it serves. A pass scans `<data>/history/`, reads each
session's `.ended` marker — never mtimes — and on an expired marker removes the whole session:
persisted media first, then the history directory. Media first is what makes a crash between
the two removals benign: the marker survives, so the next pass finishes the job; the reverse
order would strand a media directory with no clock left to age it.

Two kinds of directory are never deleted on a guess. A session the live registry still knows is
kept even if a marker exists. A directory whose marker is missing or unparsable is kept by the
pass — but at startup it is first **adopted**: a manager start owns no sessions, so an unmarked
directory can only be a leftover from a previous manager (a crash, or a shutdown that killed its
sessions). Stamping `cause=manager_restart` starts its window there instead of leaving it
unbounded, which is the one cause 08's "however caused" otherwise did not clock.

*Rationale:* one clock (the durable marker), one policy owner (the manager), one bounded pass (a
directory scan per hour, not a timer per session). The startup lines stay quiet unless the pass
removed or adopted something.
*Rejected:* inferring session end from mtimes (copying a directory resets it); a sweep on every
close (a full scan per close for no timeliness over hourly); deleting an unmarked directory (a
live session's directory would race the pass); a `retention.sweep` config key (the hour is an
implementation detail; the window is the contract).

## P4-ENDPOINT — the manager TCP surface and its gate

`manager.bind` now has its consumer: `serve` binds the unix socket as before and, when the key
is set, a TCP listener beside it (`host` or `host:port`, bare host = 8756, IPv6 in brackets).
The frame protocol is unchanged; credentials ride the frame as additive fields — `token`,
`scope`, `scope_token` (05) — presented on every call after the tokenless handshake. The
transport gate is the instance token this start adopted (no token, no network caller); scope
identity is per request and authenticated against the scope's secret file, because a claimed
string is never authentication (07). Refusal classes follow the frame's connection rule: a
token failure ends the connection (transport gate), scope failures continue (per-request
identity, pooled connections). Scope names are validated as filename components before any
path join — a network-supplied `../` could otherwise steer the secret read; the same validator
closes the pre-existing listen-bridge path, where the `scope` header reached `scope_file`
unchecked.

Startup ensures the credentials a network surface needs (instance token, one secret per scope
file present), printing paths only; the token is created only when absent so two racing
starters converge on one value. The rotation window is now enforceable on the wire: the running
manager keeps accepting the adopted value and refuses the rotated one until restart.

*Rejected:* HTTP-shaping the manager RPC (a second protocol for one surface, and the frame
already carries identity); a loopback exemption for the token (07's gate is request-time, and
the bridge already ensures a token for loopback binds); letting an absent `scope` mean
`operator` on TCP (a unix convenience becoming a network default identity); a token in the URL
(shell history and proxy logs; the frame is the carriage).

## P5-D2 — `config scope create` and its templates

`dashboard` was a pinned reserved name with no creation path: scope files were readable and
validatable only, so a fresh setup could not credential the dashboard identity without
hand-editing a file — the one step packaging cannot ask an operator to perform through
`$EDITOR`. `config scope create <name> [--template own|all|dashboard]` writes one new
`scopes/<name>.ron` (mode 0600 from the first syscall, `create_new` — an existing path refuses
with `scope "<name>" already exists`), validates the rendered text before writing, and prints
the path only. `own` is the default (own-scope visibility); `all` sets visibility `all`;
`dashboard` sets visibility `all`, audit flags false, and the default rate limit. Grants stay
empty in every template, so a template never grants capability.

The dashboard note's "reads only, no session creation" was corrected in place: the always-on
session-lifecycle set (05) is available to every scope and a scope file cannot remove it, so
the phrase describes the dashboard's own calls rather than a narrowed capability. The network
secret stays out of create — `ensure_network_credentials` already mints one per scope file at
every network start, and a second minting path would only diverge.

*Rationale:* creation is a template write, not a new configuration language; the pinned
template names carry the dashboard mapping so packaging passes a name, not a hand-copied RON
fragment.
*Rejected:* `config set scopes.<name>...` (the list-tolerant file form is deliberate — `get`/
`set` do not address scopes); an implicit `dashboard` scope created by the dashboard's first
use (the reserved name is created explicitly, never as a side effect); a `--grant` flag on
create (grants are the security-relevant part and stay a file edit under review).

## P5-PACKAGING — the bare-host unit

The bare-host channel (09) ships one systemd **user** unit, a template:
`packaging/systemd/daiorama@.service`, `ExecStart=%h/.local/bin/daiorama serve
--instance %i`. `KillMode=control-group` makes a unit stop sweep the manager's
whole session tree, and `Delegate=yes` gives the manager the cgroup subtree the
per-session limits need (06) — a service can carry both, where the `systemd-run`
scope carries delegation without the sweep (10). `Restart=on-failure` brings a
crashed manager back as an empty manager; sessions never survive a restart
(01, 10), so nothing is silently resurrected. The unit holds no `Environment=`
secrets: the token and scope secrets live in their 0600 files (07), and an
environment line would also expose them through `systemctl show`. It runs no
`ExecStartPre` directory creation because 08 single-sources directory creation
in the binary — the same rule that keeps the image entrypoint from duplicating
it.

*Rationale:* instances are first-class in 08 (per-instance config, data, and
socket), so supervision is per instance and `%i` is the only thing that varies;
a template keeps one file for N instances and names the instance in `status` and
the journal. The runbook reads the shipped unit back (a reader test pins
`serve`, `KillMode=control-group`, `Delegate=yes`, no secret) so operator-facing
wording cannot drift from an edit.
*Rejected:* a system-wide unit (needs a fixed user account and loses the user
runtime dir that 08's socket path assumes); an instance-less unit with a copied
file per instance (config drift for no gain); `daemon start` inside the unit (a
supervisor already provides detachment — nesting the detached form under it is
the layering error 10 warns about); `Restart=always` (a clean stop is a decision,
not a failure); auto-creating the noVNC tree in `ExecStartPre` (that install step
belongs to the packaging slice that ships it, and 08 keeps directory creation
in-binary).

## P5-NOVNC — the view asset tree is staged data, and `doctor` reports it

The view front has always served a static tree from `<data>/novnc` (07, `Paths::novnc_dir`),
and 09 called the assets "shipped with the core (embedded)" — one of those had to give. The
code wins: **the tree is staged data**, installed with `packaging/novnc/fetch.sh` (pinned noVNC
v1.7.0, artifact sha256 `b1003a11…`, 129 files) into the instance's data root, and the front
refuses `view_unavailable` with its pinned message until the entry point (`vnc.html`) exists.
`doctor` gains an `assets:` section that reads exactly that entry point: installed names the
path; missing prints the front's refusal verbatim plus `install: packaging/novnc/fetch.sh
<path>`. The staging script moved from the P5 sourcing note into `packaging/novnc/` — the
install mechanism belongs to the packaging channel — while `p5/novnc/README.md` stays the
sourcing record (pin, licence split, why the npm package cannot serve the front).

*Rationale:* the binary already resolves a data path per instance (08), so embedding 1.6 MiB
of MPL-2.0/BSD/OFL/CC-BY-SA assets into it would duplicate what the front already reads and
would tie licence obligations to every binary release; staging lets the tree upgrade
independently of the core version. The 503 already names the expected path, so its exit path
is one command, and the doctor line is the same string the operator sees in the browser —
wording drift between the two fails the build (the reader pin). Reading the entry point, not
merely the directory, matches the front's own test: a directory without `vnc.html` serves
nothing, so `doctor` must not call it installed.

*Rejected:* embedding the tree in the binary (asset churn couples to core releases; the front
still resolves a filesystem path, so the embed would be dead weight); installing from the npm
`@novnc/novnc` package (its `files` list omits `vnc.html` and `app/`, so the front's lookup
fails — recorded in the sourcing note); auto-staging in the unit's `ExecStartPre` (08 keeps
directory creation in-binary, and a systemd unit is not the only deployment shape); a new
`daiorama assets install` command (a download+verify surface this phase does not need — the
script is the mechanism, and releases will carry its hash in the manifest).

*Open:* whether the signed release manifest carries the noVNC tarball hash as its own row
or the installer re-derives it from the core manifest. Nothing in this slice depends on the
answer: the script's pin is fixed, and releases do not exist yet.
