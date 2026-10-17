# 04 — CLI surface

Names, flags, defaults, and refusal text are part of the contract: agents and
scripts string-match them, so they are pinned here and changed only by spec
amendment. All names are plain; no theme words (02).

Global shape: `daiorama <command> [flags]`. `--help` on every command surfaces
creation/usage options in place (D1). Read commands accept `--json`.

## Global flags and environment

| flag/env | meaning |
|----------|---------|
| `--instance <name>` | Which local instance to target. Default `default`. |
| `--remote <addr>` | Which manager to talk to: `host:port` or a literal unix socket path. One flag, one meaning, everywhere. Overrides `remote.default` for that call. `--remote` NEVER triggers a local start (B5). |
| `--json` | Machine-readable output on read/status commands. |
| `DAIORAMA_TOKEN` | Instance token for non-loopback access (exact name, pinned — nits). |
| `DAIORAMA_SCOPE` | Scope name for a stdio bridge process (05). |
| `DAIORAMA_SCOPE_TOKEN` | Scope secret for that scope, when it has one (07). |

## Commands

**Sessions** (all take `--remote`):
- `daiorama new [--backend headless|nested|host] [--resolution WxH] [--scale F] [--view] [--label NAME]` — create a session. Defaults: backend `headless`, resolution `1920x1080`, scale `1.0`. Prints the session ID.
- `daiorama close <session>` — explicit teardown. Always explicit; never a side effect (01).
- `daiorama list [--limit N] [--cursor C]` — sessions with status, scope, origin, idle-since. The CLI runs as the `operator` scope; its visibility is operator-wide (its scope file ships `sessions.visibility = all`). Prints `next_cursor` when a page truncates.
- `daiorama status [<session>]` — no argument: instance status (below). With ID: session detail (state, view state, address, idle-since, versions).
- `daiorama control <session> <action> [key=value ...]` — one control action; `<action>` is an MCP control-tool name (05); `key=value` pairs are that tool's named arguments, typed per its inputSchema; results print like other read commands (`--json` supported). Unknown action ⇒ `bad_request`, message lists the valid names.
- `daiorama view <session> [--stop]` — print the view address (starting the view if needed, same mechanism as `get_view`), or stop it. `--json` includes the per-view `credential` field (07).
- `daiorama history [<session>] [--scope <name>] [--limit N] [--cursor C]` — read the history record (own-scope default; the same scoping rule as sessions, 06).
- `daiorama media fetch <id> --out <path>` — fetch one captured media artifact (32 lowercase hex id) through the manager socket and write it to `--out` (0600, required; stdout stays clean). Claim-on-open: this fetch consumes the id, exactly like an HTTP fetch. This is the stdio bridge's fetcher for `clip_video` resource links.

**Instance management:**
- `daiorama serve` — run the manager in the foreground (container main process, deliberate terminal supervision, systemd `ExecStart`). Signals cascade to everything it owns.
- `daiorama daemon start` — start the manager detached (survives shell exit).
- `daiorama daemon end [--force]` — stop the manager. Plain `end` refuses while sessions are live and lists them; `--force` stops the manager and everything it owns (a session drop, stated in output).
- `daiorama status` — instance status: state, mode (attached/background), pid, socket path, core + protocol + `min_supported` versions, session counts, drain state. This is the single manager-status surface (no separate `daemon status`).
- `daiorama drain [--wait]` — enter drain: new sessions refused (code `draining`), running sessions reported. `--wait` blocks until zero running, bounded by `--timeout <secs>`.

**Config** (authority rules in 08):
- `daiorama config get <key>` / `config set <key> <value>` — dot-path keys, max one level of nesting. Against `--remote`: RPC to that manager. Local, no manager needed: operates on the files directly (cold). A local `config set` writes the file; a running manager keeps the old value until `config reload` (or restart) — `config set` output states which of the two applies.
- `daiorama config edit` — `$EDITOR` on the core config file. Refused against `--remote` (explicit refusal, not fetch-edit-push). Scope files are edited where they live (08); `config scope` commands are for reading them.
- `daiorama config setup` — first-run wizard: detect environment (deps, GPU via `doctor`), write starter config, offer commented starter scope files. Sequence in 10.
- `daiorama config reload` — re-read core config on a running manager (reload rules: 08). Targets a running manager; if none is running: `no manager running` — config applies at next start anyway. Never auto-starts (cold class).
- `daiorama config validate [<path>]` — validate core config and/or scope files; unknown tool/category names are load errors listing valid names (D4).
- `daiorama config scope list` / `config scope get <name>` / `config scope validate <name>` — scope defs are first-class, readable and scriptable (D4).
- `daiorama config effective [--scope <name>]` — resolved view: which values apply where, effective quotas/manifests/audit flags per scope (D4).

**Token** (lifecycle: 07):
- `daiorama token show` — print the local instance token (and its file path). Local-instance only: explicitly refused with `--remote`.
- `daiorama token rotate` — regenerate the token file. Local-instance only (refused with `--remote`). States plainly that the new token takes effect on next manager start, that the file holds the new value while the running manager still accepts the old one until then, and that restarting drops running sessions (07).

**MCP bridge:**
- `daiorama mcp [--listen <addr>]` — run the bridge. Default: stdio, targeting the local manager (`--remote` picks another — one flag, one meaning, everywhere). `--listen` runs it as a network service (auth rules in 07). This is the entry every harness config points at.

**Diagnostics and packaging:**
- `daiorama doctor` — host-runnable environment check: GPU family + exact next step, Hyprland/wayvnc presence and versions, Wayland, cgroups v2 delegation, package-manager-specific install commands. In-container it verifies the actual renderer and fails loud rather than diagnosing blindly (09).
- `daiorama dashboard serve` — serve the dashboard (module must be installed).
- `daiorama dashboard install [--version <v>] [--from <path|url>]` — install the dashboard module (mechanism: 09).
- `daiorama dashboard connect <address>` / `dashboard connections list|remove <name>` — dashboard-side manager connections (D3; flows in 10).
- `daiorama dashboard token set|remove <connection>` — store/remove per-manager tokens in the dashboard config (0600).
- `daiorama version` — core version + protocol version + `min_supported` (skew display, C3).

## Pinned defaults

| setting | default |
|---------|---------|
| instance name | `default` |
| manager socket | `$XDG_RUNTIME_DIR/daiorama/<instance>/manager.sock`; pinned fallback when the runtime dir is unusable (08) |
| manager TCP port (when bound) | `8756`; non-loopback requires token (07) |
| bridge network listen | `127.0.0.1:8757`; non-loopback requires token + scope auth (07) |
| view front port base | `6080`, allocated upward per active view (06) |
| resolution / scale | `1920x1080` / `1.0` |
| idle reaper | off (06, rationale in 11) |
| dead-session sweep | `reaper.sweep_dead = on` — independent of `reaper.idle` (06) |
| history | 8 segments × 4 MiB per session (text); retention 7 days from session end (08) |
| token file | `<data-dir>/<instance>/token`, mode 0600 (08) |
| media TTL | `media.ttl = 600` (seconds) or fetch-once, whichever first (05) |
| manager start mode | `auto` (attach/background heuristic, 10); pin via `manager.start_mode = auto|attached|background` |
| renderer software override | `renderer.allow_software = off` (06; config-only, never reachable at runtime) |
| drain `--timeout` | 300 (seconds; bounds `drain --wait`) |

## Machine-readable output (`--json`)

`--json` prints the RPC response shape unmodified (shapes pinned in 05);
errors emit the `{code, message, hint?}` envelope verbatim; a cold answer
emits `{"code": "no_manager_running", "message": "no manager running"}` with
exit 0 — it is an answer, not an error.

## Value grammar

Config values: integers; durations `<n>s|m|h`; enums literal (`off`, `auto`,
`attached`, `background`, `pace`, `reject`, `own`, `all`); booleans `on|off`.

## Cold vs hot (auto-start classification, B5)

"Hot" = **may-start** — may lazily start a local manager when none is
reachable. "Cold" = **never-starts** — answers from files/void, even when a
manager is running (e.g. `config reload` RPCs a running manager but never
starts one). With `--remote`: nothing local is ever started; unreachable remote
is an error, never a local fallback.

| class | commands |
|-------|----------|
| cold | `list`, `status`, `close`, `control`, `view`, `drain`, `daemon end`, `config get/set/edit/reload/validate/setup`, `config scope *`, `config effective`, `history`, `media fetch`, `version`, `doctor`, `token show`, `token rotate`, `dashboard install/connect/connections/token` |
| hot | `new` |
| neither | `serve`, `daemon start`, `dashboard serve`, `mcp` (explicit process runners) |

A cold command against "no manager" is an answer, not an error: `no manager
running`. When a hot command lazily starts one it prints one line: `started
manager (mode=<attached|background>, pid=<pid>) socket=<path>` (B5). With
`manager.bind` set, startup prints one more line naming the address it actually
bound: `listening on <addr>` (07).

## Refusal and error strings

Canonical, shared verbatim by CLI, bridge, and dashboard (D2). Operators grep
these; wording changes are spec changes.

- `manager unreachable at {addr}` — the shared reachability string. Never followed by a local-start attempt when `--remote` was given.
- `no manager running` — cold commands' honest answer.
- `Hyprland not found. Install it: {resolved command}, or use the container image.` — `{resolved command}` comes from the maintained install table (09); where the distribution has no package it renders "not packaged for this distribution; see the release notes for a COPR/upstream build".
- `Hyprland {found} is outside the supported range {min}-{max}.`
- `session refused: active renderer is {renderer} (software). This session would carry a known automation fingerprint. Run 'daiorama doctor'.`
- `refusing to bind {addr}: non-loopback binds require a token; none could be generated at {path}.`
- `containment unavailable: cgroup v2 delegation is not available here ({detail}). Fix: run with a writable/delegated cgroupfs (see the compose notes), or set limits.mode = off and accept losing per-session limits and crash-time cleanup.`
- `token required for {addr}: set DAIORAMA_TOKEN (value from 'daiorama token show' on the manager host).`
- `manager.bind takes a host or host:port address` — a malformed bind refuses the start (07).
- `invalid scope name` — a network-supplied `scope` that is not `[a-z0-9][a-z0-9_-]{0,63}` (07).
- `session limit reached: scope '{scope}' {n}/{max}; global {g}/{gmax}. Close a session first.`
- `backend 'host' is exclusive; in use by session {id}. Close it or use --backend=headless.`
- `manager is draining for upgrade; new sessions are refused ({n} still running).`
- `protocol mismatch: this client speaks {cv} (min {cm}); manager speaks {mv} (min {mm}).`
- `scope required: set DAIORAMA_SCOPE.` (stdio bridge started without a scope)
- `module not installed; run 'daiorama dashboard install'.`
- `view unavailable for {session}: {reason}.` — wayvnc unusable, the view-port range exhausted, or the front bind refused.
- `no session specified; create one first (tool: create_session).` — MCP-side form of the missing-ID error.

Final wording is frozen at sign-off; that is the last moment wording may change freely.
