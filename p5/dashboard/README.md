# Dashboard seams — pre-read (P5)

The dashboard is an optional, separately installed web UI that aggregates sessions/history
across managers (02, 03). By rule the manager has **zero dashboard-aware code**; the dashboard
is a plain pull-only client of the manager API (03, B10). This note inventories the seams that
already exist, the ones that are still open, and the questions packaging (09) and the dashboard
slice have to answer before code.

## What exists

- **Identity and transport.** The `manager.bind` TCP surface with the instance token plus the
  per-request scope credential (`07`, P4-ENDPOINT) is the path a remote dashboard uses. A
  dashboard holds one token per manager (04: `dashboard token set|remove <connection>`) and
  names the scope it queries as.
- **Read surface.** `status` (instance state, versions, session counts, drain), `readiness`,
  `list_sessions` (scoped, paginated), `session_status`, `view`, and `history` (scoped, opaque
  cursors) are everything a v1 dashboard needs for a session/history pane. No new RPCs required
  by this pre-read.
- **Module field.** `status`/`readiness` carry `versions.dashboard_module?` (05) — the manager's
  report of the installed dashboard module version.
- **Install mechanism (09).** `daiorama dashboard install` downloads a version-matched module
  artifact into `<data>/modules/daiorama-dashboard/<version>/` and activates it by atomic
  symlink swap; `dashboard serve` refuses with the pinned string when absent. `dashboard *` is
  currently `not_implemented` in the CLI — that is the implementation surface, not a design gap.
- **Reserved query scope.** 10 (D3) pins that the dashboard queries as the reserved `dashboard`
  scope, not as `operator`.

## Pinned decisions (v1)

These are the decisions packaging and the dashboard slice build against; each is deliberately
small and keeps the manager free of dashboard-aware code.

### D1 — Who fills `versions.dashboard_module`

The manager reads the **activation symlink**, never a config key and never a client-supplied
path: `<data>/modules/daiorama-dashboard/active` → a version directory under the same parent.
It is read when `status`/`readiness` answer (a `readlink` per call, no cached state), so an
install or revert is visible without a restart.

- Version component must match `[A-Za-z0-9._-]{1,64}`, not `.`/`..`, and the joined directory
  must exist; anything else is `None`.
- Absent module, dangling link and a link pointing outside the module parent are all `None` —
  `status` omits the field rather than refusing.
- The manager never writes under `modules/`; activation belongs to `dashboard install` (09).
- Packaging provides the link name `active` and the `<version>/` directory layout; 09 already
  pins the directory form and the atomic swap.

### D2 — The reserved `dashboard` scope

`dashboard` is a **reserved scope name**, like `operator`: it is created explicitly, never
implicitly, and it is the identity the dashboard queries as (10, D3).

- `config scope create <name> [--template own|all|dashboard]` is the creation path (04, 08).
  Default template `own`; `dashboard` maps to `(sessions: (visibility: all))`
  with audit flags false and the default rate limit. The always-on session-lifecycle set (05)
  is available to every scope and is not removable by a scope file, so "reads only" describes
  the dashboard's own calls, not a capability the template takes away.
- Refusals: invalid name → `bad_request` `invalid scope name`; existing file → `bad_request`
  `scope "<name>" already exists`; the command writes a 0600 file and prints the path only.
- The network secret is not written by `config scope create`: the next network start mints a
  secret for every existing scope file (P4 behavior), so no hand-editing is needed.
- `operator` and `dashboard` are the reserved names; neither can be created twice, renamed or
  removed through the CLI in v1 (no such verbs exist, and none are added here).

### D3 — Dashboard-side connection store

- Path: `<config root>/daiorama-dashboard/connections.ron`, file **0600**, directory 0700.
- Shape: `(version: 1, connections: [(name: "...", address: "host:port", token: "...")])`.
  `address` uses the same grammar as `--remote`; the unix socket is deliberately not a
  dashboard address (07: a network surface with a token is the remote path).
- `dashboard connect <address> --name <name>` appends/updates; `connections list` prints name
  and address only; `token set <connection>` reads the value from **stdin** (never argv, so it
  stays out of shell history and `ps`) and never echoes it. Values are never printed by any
  verb, matching the manager token rule.
- No env-var token path (10 D3): N connections must scale without ambient secrets.

### D4 — Capability detection

- The protocol semver range is the contract: the dashboard performs the normal `handshake`,
  treats a `version_incompatible` answer as "cannot use this manager" and shows the pinned
  message, and otherwise assumes the full core surface for that protocol.
- `versions.dashboard_module` is **informational**, never a capability gate: a manager without
  the module still serves sessions/history; the dashboard only uses the field to show whether
  the module is installed there.
- Unknown fields in `versions` are ignored; no per-feature probing in v1 (a probe surface would
  become a second contract). If a future feature needs detection, it adds a `versions` field,
  not a trial call.

### D5 — Artifact and install ordering

- Release asset: `daiorama-dashboard-<version>-<target-triple>.tar.gz`; manifest row kind
  `dashboard` with `{version, triple, sha256}`, under the same signed manifest as core (09).
- `dashboard install` verifies the manifest checksum before extraction, extracts to a staging
  directory under `modules/daiorama-dashboard/`, then renames and swaps `active` atomically;
  the previous version directory is kept for `--version <old>` reverts.
- **Ordering:** packaging lands (a) the manifest row/asset naming, (b) `config scope create`
  and (c) the install skeleton before dashboard UI code; the dashboard consumes these pins.
- Skew handling: `dashboard serve` compares its own build version with the active module
  version (D1). Absent → the pinned 04 refusal (`module not installed; run 'daiorama dashboard
  install'.`, code `module_not_installed`). Mismatch → `bad_request` with
  `dashboard module <active> does not match this dashboard (<build>); run 'daiorama dashboard
  install --version <build>'`. No new envelope code in v1.

## Recommendation

Keep v1 dashboard work to (1) the module-version source and (2) the scope-creation path — both
small, both manager-side-adjacent, neither making the manager dashboard-aware. The dashboard UI
itself is a separate client and can be built against the existing read surface without core
changes.
