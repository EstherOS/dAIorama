# 03 — Architecture

## Components

Four pieces. How they relate matters more than any single piece.

**CLI (`daiorama`)** — the operator/client surface. Every command that talks to
a manager is a thin RPC call against it over a local socket; nothing stateful
lives in the CLI. (04 pins the surface; 10 pins start/stop semantics.)

**Manager** — the one persistent process per instance. It is the only daemon
that exists; sessions, control, config, token, and history are all calls against
this one process, not separate services. It is lazily started by the first
command that needs one (classification and start semantics in 04, 10).
It owns:
- the session registry (what exists, IDs, status) — the single source of truth;
- an append-only history log of sessions and control calls, tagged by origin
  (shell or MCP) and scope, written continuously whether or not anyone reads it;
- the compositor and view processes it forks (below).

**MCP bridge** — what a harness connects to: MCP on one side, manager socket on
the other. Deliberately stateless with respect to session lifetime; a bridge
crash or harness restart costs nothing, because sessions belong to the manager.
It runs either the classic way (spawned per connection over stdio) or as a
long-lived network service (05).

**Dashboard** — a deliberately separate, later, fully optional component. It is
built entirely on the same generic query API other clients use (list sessions,
session detail, history) and talks to managers as just another client. The
manager has zero dashboard-aware code. v1 direction: dashboard-initiated pull
only (B10); a manager-dials-dashboard reporting mode is a separate future
mechanism, not denied in prose and not built now. Packaging in 09, operator
flows in 10.

## Ownership seams (normative)

- **Parent death guarantee.** The manager forks its direct children (compositor,
  view processes) and sets a non-ignorable parent-death signal (`SIGKILL`)
  immediately after fork, before exec, with the child re-checking `getppid()`
  after setting it (the prctl-after-fork window; implementation note: the
  signal binds to the forking *thread*, so children are forked from the
  process whose death must trigger them). This kernel-backed guarantee
  applies to **direct children only**: everything the compositor execs
  (browsers and their process trees) is a grandchild and is covered instead by
  the per-session cgroup kill path (06). Crash and close paths run `cgroup.kill`
  on the session's cgroup — the completion guarantee — with the parent-death
  signal as the belt to that suspenders.
- **Empty after restart.** Because of the above, the session registry always
  starts empty after the manager restarts. Nothing is reconciled; nothing could
  be. This is a consequence of the guarantee, stated so nobody re-derives it.
- **Startup sweep (the one thing that is cleaned).** Session cgroups live under
  an instance-owned subtree. On manager start, before binding, the manager
  sweeps that subtree: session cgroups left by a previous manager run are
  `cgroup.kill`ed. The registry cannot be reconciled — sessions died with their
  manager — but cgroups and orphaned process trees can be, and are; that is the
  only reconciliation the design promises. When the subtree is absent (e.g.
  `limits.mode = off`), the sweep is a no-op.
- **Lazy-start atomicity.** Startup of the manager takes an atomic lock (flock,
  kernel-released on crash) before anything binds or starts anything: exactly
  one manager per runtime path; losers wait briefly (bounded) and connect,
  retrying once if the winner died pre-bind (10).
- **Health ≠ liveness.** The manager periodically checks that the compositor's
  IPC socket answers a trivial query, not merely that a process exists. Action
  on failure: 06.

## Compositor and desktop

Hyprland is the compositor (decision + rejected alternative: 11). It was chosen
over niri for today's needs — documented, stable headless operation; complete
virtual-input protocol support (`zwp_virtual_keyboard_manager_v1`,
`zwlr_virtual_pointer_manager_v1`); existing prior art for this exact use case.
niri remains the documented alternative; in containers the nested-compositor
workaround is the demonstrated shape (P1 L1, `p1/`).

Rootless input injection is why this is a Wayland design: wlroots-family
virtual-input protocols grant pointer/keyboard control to any client holding a
plain socket connection to the compositor — no `/dev/uinput`, no elevated
capability, no root, ever. Access control lives in the compositor's own policy,
and since dAIorama owns the entire session, that policy is simply "any client
on this socket has full access".

## Backend modes

Selected at session creation via `--backend=headless|nested|host`; default
`headless`. These are properties of the compositor, not of containerization.
Resolution and scale are creation parameters too (defaults in 04), because
screen geometry is its own fingerprint; changing them mid-session is out of
scope (01).

- **`headless`** (default): no physical display. In containers, native
  standalone start is not available with aquamarine 0.15.1 — the backend
  requires a primary allocator (a `drmFD>=0` impl); the DRM path enumerates
  `card*` only and opens via libseat, which needs an active seat; `renderD*`
  is never enumerated; `AQ_NO_KMS_REQUIREMENT=1` relaxes only the KMS gate
  (`AQ_DRM_DEVICES` pins the node where several exist). Demonstrated shape
  (P1 L1, `p1/`): an invisible minimal parent (mutter `--headless`; parents
  must advertise `xdg_wm_base >= 6` — weston/sway are rejected at bind)
  running Hyprland nested, with dedicated virtual outputs created on demand
  (`hyprctl output create headless <name>`) for resolution and scale. The
  wayvnc capture path is verified; the resize/scale path resolved in P2
  (`p2/pre-w2/resize-scale/`: mode/scale matrix + live resize, captures
  verified; the spawner implements `(logical, scale) → mode`; 09).
- **`nested`**: runs as an ordinary client window inside an already-running
  outer Wayland compositor (detected via `WAYLAND_DISPLAY`). Lets a human watch
  the bot's desktop as a window, no VNC involved.
- **`host`**: the real DRM/TTY backend against actual display-capable or
  render-only hardware. **Scoping (B6):** the "never needs the display node"
  claim holds for `headless` and `nested` only. `host` *is* the DRM/KMS path:
  it needs a seat / display node, is effectively bare-metal only (containers
  lack the display node), and its rootless story is unproven. It is one
  session at a time **per device, not per manager** — exclusivity is
  device-global: creation takes an flock on the DRM card node itself and is
  serialized per instance; a second `new --backend=host` (this manager or
  another) fails with the incumbent session named (error string, 04).

## GPU access

GPU access is a hard requirement (the compositor has no **supported**
software-only fallback path; the llvmpipe CI lane is the deliberate exception,
09) and must work rootless. The load-bearing distinction: a GPU exposes
`/dev/dri/card*` (display/KMS node; real use needs DRM master, historically
root/logind) and `/dev/dri/renderD1xx` (render node; unprivileged by design,
governed by ordinary group permissions). dAIorama needs only the render node for
`headless`/`nested`. Container passthrough specifics, per GPU family, live in 09.

Normative consequences:

- **No hardcoded device path anywhere in the code or shipped image.** GPU
  device selection is a single config value: a render-node path **or** a CDI
  device identifier.
- **Detection is a host-runnable command, not a blind in-container step.**
  `daiorama doctor` (04) detects the family and prints the exact next step per
  host; container startup only verifies the actual renderer and fails loud.
- **Software rendering is a correctness bug, not a performance issue.** A
  session silently running on `llvmpipe` carries the exact automation
  fingerprint this project exists to avoid. The fail-loud policy (refuse at
  creation; re-verify at first browser launch) is specified in 06 (D6).
