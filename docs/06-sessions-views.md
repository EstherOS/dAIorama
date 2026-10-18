# 06 — Sessions and views

## Creation

Sessions are created explicitly (`daiorama new` / MCP `create_session`) and
closed explicitly. IDs are stable, globally unique (UUIDv4), and never reused
across restarts; labels are mutable and never identifiers (02). Session
records and create results carry the manager's `instance_id`, so a cached ID
from a previous manager run reads `session_not_found` (hint: previous manager
run) at worst — never a different desktop. Creation parameters: backend,
resolution, scale, view, label — defaults pinned in 04. There is no default
session, ever, and no implicit creation from an invalid/missing ID (01,
principle 3). Each session gets a generated compositor config (resolution/
scale/env) under the session runtime dir, written before exec; runtime-only
changes go through the control surface's dispatch (09).

**Renderer policy at creation (D6, decided here):** if the active renderer is
detected as software (`llvmpipe` or equivalent), creation **refuses**:
`renderer_refused` with the string in 04. `renderer.allow_software` (manager
config, default off, no CLI flag, never reachable at runtime): when on,
creation proceeds on a detected software renderer and the session is marked
`renderer: software` in its records — the override exists for protocol-test
lanes only.

If the renderer cannot be verified at creation time, creation proceeds with an
explicit warning and the session is marked `renderer: unverified`. The
compositor-side check is primary; the browser-side re-check has an executor:
browser launches go through the session launcher (shipped in the desktop image at
`/usr/local/bin/daiorama-browser`; 09 — the documented launch route), which
captures the browser's own rendered-GL renderer string from the **compositor-reported
window title** of a probe page (measured; Chromium's stderr carries no unmasked
renderer at any verbosity — `p5/browser/evidence/channel-probe.txt`) into a
manager-readable file in the session runtime dir; the manager reads it on session
detail, and a software result flips the mark one-way to `renderer: software` and
appends a `renderer.software` history record carrying the string. The title must
survive long enough to observe: the probe holds it for 2 s before navigating, and
the launcher's acceptance leg measures that window rather than assuming it.
Where the browser bypasses the launcher (a bare-host browser that was never
routed through it), the session stays `renderer: unverified` — it is never
silently marked verified. Quiet degradation into a fingerprinted session is
the one failure that must never happen (03).

**Host exclusivity (B6):** `host` backend is device-global exclusive. Creation
takes an flock on the DRM card node and is serialized per instance; races
(concurrent `new --backend=host` anywhere on the host) resolve by that lock,
and the loser fails with `backend_busy` naming the incumbent session (string
in 04).

## Session state machine (B3)

```
running → degraded → dead → closed
```

- **running** — compositor alive and healthy (IPC socket answers a trivial
  query; process existence is not health).
- **degraded** — health checks failing; the kill path is engaged. Transient;
  never a resting state.
- **dead** — the tree has been killed via `cgroup.kill` (the completion
  guarantee for grandchildren, 03), resources freed: GPU, cgroup, any view.
  The entry remains listable and inspectable until closed.
- **closed** — registry entry removed. Final. Only `close` (operator/agent) or
  the reaper's dead-session sweep produces this.

**Health-failure action, exactly:** kill the process tree through the session's
cgroup, mark `dead`, no auto-restart (masking crash loops is prohibited — a
fresh explicit `new` creates a clean session, and that is the recovery path).
The dead state is precisely where zombies would accumulate, so:

- **Who closes dead sessions:** the dead-session sweep — `reaper.sweep_dead =
  on`, its own default, independent of `reaper.idle` (sweeping a dead entry
  changes nothing operators rely on) — closes them on its next pass; an
  operator or agent may close one immediately. Dead sessions never wait on an
  idle timer — death is detected, not waited for.
- **Quota accounting:** a dead session holds no live resources and does **not**
  count against `max_sessions` or the global ceiling; it is reported as
  `dead` (alive/dead shown separately). The registry slot is freed when
  closed, which the sweep makes prompt. Rationale and the rejected alternative
  (dead counts until closed): 11.
- **Tombstones:** closed/reaped IDs are remembered for 24 h (bounded; cleared
  on manager restart): calls inside the window get the distinct code
  (`session_closed` / `session_reaped`), outside it `session_not_found`. A
  reap marks the entry dead with cause `reaped` (calls while the entry lives
  return `session_reaped`); the sweep closes it.

**Crashes:** compositor dies mid-session → same treatment as a failed health
check: mark dead, never auto-restart (§ crash policy). A view process dying is
*not* this: view down ≠ session dead (below).

## Views (B2)

A view is one wayvnc process plus the manager-owned front (07) serving its
noVNC page. Zero or one per session.

**Registry.** Views are first-class registry entries:
`{ state: starting | running | down, address, front_port, vnc_port,
generation }` — `front_port` allocates from `view.port_base`; wayvnc gets an
ephemeral loopback port (manager-known; not part of the address contract).
The manager owns view state the same way it owns session state — no view
state lives outside the registry.

- **Port allocation:** at **view start** (not session creation — lean
  default), from `view.port_base` (6080, 04) upward, first free port;
  released on view death or stop. Several viewed sessions coexist on
  consecutive ports; the old single-"VNC port" compose assumption is dead.
- **One start path, serialized:** a per-session lock wraps view start, so
  concurrent `get_view`/`--view`/dashboard starts cannot fork two wayvncs;
  losers get the winner's address.
- **Generation:** increments on every view start. `get_view` and session
  detail return the current address + generation, so a cached address from
  before a view crash is detectable as stale (generation moved) instead of
  silently dead.
- **View death:** mark view `down`, leave the session running. The next
  `get_view` starts a fresh view (new generation). Hyprland death (session
  dead) kills the view with it — explicit kill-on-session-death, no lingering
  wayvnc holding a port for a dead session.

## The idle reaper (B1)

Off by default (`reaper.idle = off`). When enabled with a duration:

- **Activity** is: any session-scoped RPC, **or** an attached view actively
  streaming, **or** compositor input events in the session. The old
  "no control calls" definition reaped exactly the sessions in live use
  (watching a stream sends zero control calls) — fixed by definition.
- Watched sessions are exempt from the control-call timer; an optional
  separate `reaper.view_idle` applies to watched-only sessions (off by
  default).
- `idle-since` is surfaced in session detail and list.
- A reap is not a close: the session ends with cause `reaped` (error code
  `session_reaped`, history entry carries the cause). It never fires while a
  caller is mid-call on that session.
- Default off per lean-by-default; 11 records the recommendation to enable it
  (≈1 h) on shared/central shapes, with rationale.

## History (B8)

Append-only log of sessions and control calls. Records carry **scope**, origin
(`shell` | `mcp`), timestamps, and cause where relevant; control-call
arguments are not recorded. Reads obey the same scoping rule as sessions
(own-scope by default; per-scope `sessions.visibility`, 08). The dashboard is a **named
scope** with an explicit, documented broad-read grant — configured deliberately,
never default. Storage mechanics (buffered writes, rotation, retention,
defaults) live in 08; wear rationale in 11.

## Per-session resource limits (C6)

Memory and CPU limits per session via cgroups v2, so a runaway browser tab gets
its own processes killed by the kernel while the manager and every other
session survive. This is the containment fix for transient failures; it is why
there is no retry/restart policy layered around crash handling.

**Preconditions, stated rather than assumed (this is where silent absence
happens):** unified cgroup v2 hierarchy; systemd user delegation — `memory`
and `tasks` are delegated by default, `cpu` usually is not, and distro variance
is real; inside a container, creating sub-cgroups needs cgroupfs writable,
correct ownership, and sometimes an SELinux boolean; on bare host, creating
limits in the manager's own populated cgroup needs a delegated scope
(`systemd-run --user ... -p Delegate=yes`, documented path).

**The manager detects effective delegation at startup.** The check runs at
manager startup, after the runtime lock, before the manager binds anything;
failure fails the start with `limits_unavailable` (string in 04) — every
lazy-start racing the winner surfaces the same error; no retry loop (the
failure is deterministic). `daiorama doctor` performs the same check and
prints the exact wiring command for the container shape. `limits.mode = off`
is the explicit, recorded downgrade — and it is honest about its cost:
without session cgroups there are no per-session limits and no crash-time
`cgroup.kill`; cleanup reduces to the parent-death signal (direct children),
so grandchildren may outlive a manager crash. The startup sweep is a
containment-on feature (03): it operates on the instance's session-cgroup
subtree, which exists only while cgroups are in use. Precondition verification
is part of the spike's exit criteria (09).
