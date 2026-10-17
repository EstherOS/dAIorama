# 10 — Operations runbook

## Starting the manager (B5 — deterministic, stated)

Three paths, one process, no guessing:

- `daiorama serve` — attached foreground. **systemd units and container main
  processes always use `serve` explicitly**, never the auto-estimate.
- `daiorama daemon start` — explicitly detached (survives shell exit).
- **Auto path** (neither was invoked, a hot command needs a manager): the mode
  is chosen deterministically — attached when this looks like a container main
  process or a fresh interactive shell with nothing else running; background
  otherwise. The choice is **reported on startup** and pinnable: `manager.start_mode = auto | attached | background` (client-local key, 08). The report line is pinned in 04.

Lazy-start mechanics (normative): an **flock** (kernel-released on crash — not
an `O_EXCL` stale lockfile) is taken before anything binds; the loser waits a
bounded time and connects instead; if the winner died pre-bind, the loser
**retries exactly once** as a starter. Exactly one manager per runtime path.

`--remote` never starts anything locally: a dead remote is
`manager unreachable at {addr}` (04), full stop.

**Supervision (bare host):** run the manager as a **systemd user service**
(`KillMode=control-group` default — unit stop sweeps the whole tree).
`systemd-run --user … -p Delegate=yes` is the cgroup-delegation path and a
*scope*: it does not sweep on manager death — say so when using it.
`daemon start` (no supervisor) is dev convenience (terminal-scoped, below).
The shipped unit is `packaging/systemd/daiorama@.service` — a service with
`Delegate=yes`, so one unit carries both the sweep and the delegated subtree;
the `systemd-run` scope stays the ad-hoc path. Install and upgrade steps are in
`packaging/README.md`.

## Stopping

- `daiorama daemon end` — refuses while sessions are live and lists them.
  Plain `end` never takes desktops down silently.
- `daiorama daemon end --force` — stops the manager and everything it owns;
  output states what is being dropped (it is a session drop, by design — 03
  ownership seams). Same meaning on bare host and in containers.
- `daiorama drain [--wait] [--timeout <secs>]` — drain state: new sessions
  refused (`draining`, 05), running sessions reported until zero.

## Upgrade procedures (C3 — procedure, not a warning)

Versions are visible: readiness (05), `status` (04). `daiorama version` prints
core + protocol. Client side displays skew with the pinned mismatch string.

- **Bare host:** `drain [--wait]` → `daemon end --force` (drops whatever
  remains — drain as much as patience allows first) → install new binary →
  `serve`/`daemon start` → `status` confirms versions. Sessions do not survive;
  that is the documented tradeoff (01), stated to the operator at each step.
- **Compose:** drain → stop/recreate the container on the new digest → start.
  The recreate *is* the restart; same drop semantics.
- **Central/shared:** same sequence, plus the etiquette that matters: upgrade
  while agents are active only deliberately. There is no hot-swap and none is
  planned (01 non-goal) — re-linking PDEATHSIG children across processes is
  real complexity for a benefit this scale doesn't need.
- **Dashboard skew:** the dashboard detects capabilities per manager and
  degrades those panes; it never rejects a manager (05, 09).

## First run (D1 — the sequence, in order)

1. `daiorama doctor` — environment truth: GPU family + exact next step,
   Hyprland/wayvnc presence + versions, cgroups v2 delegation, package-manager
   install commands where things are missing.
2. `daiorama config setup` — writes a starter `config.ron` (commented),
   offers a commented starter scope file.
3. Point the harness at `daiorama mcp`; set `DAIORAMA_SCOPE` (05).
4. `daiorama new` — first session; creation options are visible in
   `new --help` (backend, resolution, scale, view) rather than tribal knowledge.

Refusals along the way use the pinned strings (04) — every one names the next
action. If Hyprland is missing on a bare host, `doctor` says so with the exact
install command and points at the image; that is the strongest signal the
image is the better path for that user.

## View assets (noVNC)

The view front serves a static noVNC tree from `<data>/novnc`; until that tree
exists every view answers `view_unavailable` with `view assets not installed
(expected <data>/novnc)` (07). Staging is one command per instance, run once
(the desktop image pre-seeds the volume instead: it carries a read-only copy at
`/usr/share/daiorama/novnc` and `packaging/container/entrypoint.sh` copies it
into `<data>/novnc` only when the entry point is absent — an image may pre-seed
the volume, never serve an ephemeral layer):

    packaging/novnc/fetch.sh <data root>/novnc

The script is pinned (noVNC v1.7.0, artifact sha256 checked before unpacking)
and replaces an existing tree atomically, so re-running it is the upgrade path.
Offline: pass the tarball as the second argument. `daiorama doctor` reports the
tree under `assets:` — the path when installed, the front's refusal plus this
command when not. That refusal is the 503's whole exit path: nothing else is
needed, and a half-staged tree is never served.

## Diagnostics and logs

- `daiorama doctor` — the diagnostic command (also in-container: verifies the
  active renderer, fails loud, doesn't guess).
- Manager diagnostics: stderr → journald (systemd user service) / the
  container's main-process logs. No bespoke log files. Sanctioned detachment:
  the service unit or a container `serve`; `daemon start` detaches with
  terminal-scoped stderr — if the terminal closes, its diagnostics are gone
  (dev convenience, stated).
- History: the domain record, in the data dir (06, 08). Not tamper-evident;
  never presented as forensic (07 posture).

## Token operations (07 tie-in)

- `daiorama token show` — value + path (local file read).
- `daiorama token rotate` — regenerates; new token applies at next manager
  start; output states that the file holds the new value while the running
  manager still accepts the old, and that restarting drops sessions. Rotating
  on a quiet instance: rotate → restart → update client env vars.
- **Backups:** `/config` (config + scopes) and `/data` (token, history,
  modules) cover an instance. The token file is a secret — backing it up is
  backing up a credential; say so in ops docs, don't stash it in shared
  archives.

## Dashboard operator flows (D3)

- `daiorama dashboard serve` — explicit start; the dashboard never appears as
  a side effect of `--view` (03).
- Connections: `dashboard connect <address>`, `dashboard connections
  list|remove` (04). Per-manager tokens live in the dashboard config (0600,
  `dashboard token set`), not env vars — N connections must scale without
  environment sprawl, and rotation is per-connection.
- The dashboard queries as the reserved `dashboard` scope: an explicit,
  documented broad-read grant — deliberate config, never default (06, B8).
- **Take-over** is explicit input arbitration: states `agent | human | shared`;
  take-over moves human-ward, release moves back, both explicit; nothing
  switches silently mid-call. Defined here so the button has a contract.
- **Rename** = label-only; IDs are stable and never change (02).
- The UI surfaces **view down ≠ session dead** distinctly (06) — a VNC crash
  must never read as a dead desktop, and vice versa.

## Watching the system

- External watchers (uptime checks, the dashboard's own status pane) poll the
  `readiness` call (05 — the RPC form of `status --json`); humans use
  `daiorama status` (04). One readiness contract, not per-integrator probing.

**Interop trust note:** mounting the runtime directory into another container
grants that container **instance trust** with cooperative scope claims — a
deliberate decision, not a side effect; shared instances point clients at TCP
(07).
