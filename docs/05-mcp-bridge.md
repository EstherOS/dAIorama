# 05 — MCP bridge and tool surface

## The bridge

Entry point: **`daiorama mcp`** — one entry, two modes; this is the command
every harness config points at (pinned).

- **stdio** (default): spawned per connection by a harness, ephemeral, dies
  with the connection. Scope arrives from the spawn environment:
  `DAIORAMA_SCOPE` (+ `DAIORAMA_SCOPE_TOKEN` if that scope has a secret); a
  bridge started without a scope refuses to run: `scope required: set
  DAIORAMA_SCOPE`.
- **network service** (`--listen <addr>`): one long-lived bridge, many
  connections. Scope is authenticated **per connection**; no anonymous calls;
  no default scope. Scopes without a secret are usable via stdio only (07
  states the model; rationale in 11).
- **network credentials (pinned):** a network MCP endpoint carries credentials
  as request headers — `Daiorama-Token` (instance token), `Daiorama-Scope`,
  `Daiorama-Scope-Token`. The bridge relays them unmodified — it is a
  presentation relay, not a token authority; the manager validates on every
  call, the same path for both hops. Failures are MCP-level errors
  (`token_required`, `scope_auth_required`, `scope_unknown`) — never an OAuth
  challenge or discovery metadata. Harnesses without custom-header support use
  the client-only install beside themselves: stdio `daiorama mcp` with
  credentials in env (`DAIORAMA_TOKEN`, `DAIORAMA_SCOPE`,
  `DAIORAMA_SCOPE_TOKEN`).
- **Stateless w.r.t. sessions, always.** Bridge crash or harness restart costs
  nothing; sessions belong to the manager.
- **Reconnect, don't die:** bridge→manager and dashboard→manager connections
  reconnect with backoff on drops and surface the shared `manager unreachable
  at {addr}` state (04), never a raw connection error. On reconnect nothing
  needs re-syncing — session state structurally cannot be lost unless the
  manager itself died (03).
- **MCP revision (pinned):** the bridge implements MCP revision 2026-07-28
  (per-request `_meta` version declaration; `server/discover`) and the legacy
  `initialize` negotiation for earlier revisions; on stdio it probes with
  `server/discover` first. `UnsupportedProtocolVersionError` (-32022) maps to
  `version_incompatible` with both pairs in the message.

**Stdio conformance surface (P3 W1).** The bridge answers the handshake paths and the
manifest locally, and relays everything else:

- `server/discover` and `initialize` answer
  `{protocolVersion: "2026-07-28", capabilities: {tools: {listChanged: false}},
  serverInfo: {name: "daiorama", version: <binary version>}}`. Neither authorizes
  anything.
- `tools/list` answers `{tools: [{name, description, inputSchema}]}` — exactly the
  caller scope's exposed manifest (the always-on set plus grants), never the whole
  registry.
- `tools/call` gates the name first: a name outside the registry is `bad_request`
  (`unknown tool "<name>"`); a known name the scope does not hold is
  `tool_not_granted`; a granted name is relayed to the manager. Tool failures are
  MCP tool-execution results (`isError: true`, envelope in `structuredContent`);
  JSON-RPC errors are reserved for malformed MCP requests.
- `list_capabilities` is answered from the scope file plus the manager's own
  filtered listing: `{tools, categories, always_on, quotas: {sessions: {max,
  live}}, audit}`, paginated over `tools` with `next_cursor` (cursor/limit per
  §List, history, and retries).
- A notification (no `id`) never executes and never gets a response.

## Scope and origin carriage on the RPC

Every manager request may carry two additive frame fields beside
`method`/`params`/`idempotency_key`:

- `scope` — the caller's application-layer identity (07). Absent means
  `operator`, the CLI's local path. The manager records it on sessions it
  creates, enforces that scope's `sessions.max` (scope files, 08 D4, read per
  call), and filters `list_sessions` by its `sessions.visibility` (`own` sees
  only its own sessions; `all` is operator-wide). A session outside the
  caller's visibility answers `session_not_found` from
  `session_status`/`close_session` — existence is not leaked.
- `origin` — `shell` | `mcp`, where the call came from (06 B8). Absent means
  `shell`; anything else is a malformed request.

Scopes with no file keep the pinned defaults: `operator` reads operator-wide,
every other scope is own-sessions-only with no quota override. The handshake
stays tokenless and carries neither field; the bridge sets both on every call
after it.

## Tool exposure model

Two questions, kept separate in design and code:

1. **Internal capability registry** — what dAIorama can do. Grows toward the
   full useful Hyprland IPC range: window management, input, clipboard, screen
   observation, query, program launch. No artificial cap once the plumbing exists.
2. **Exposed manifest** — what a specific connected caller may call. Per-scope,
   configurable by category or individual tool. Default: **deny-by-default** —
   nothing exposed until a scope's config grants it.

**One named exception to deny-by-default:** the always-on session-lifecycle set
— `create_session`, `close_session`, `list_sessions`, `session_status`, and
`list_capabilities` — scoped to the caller's own sessions by the normal scope
rules. Without it, an agent with nothing granted cannot even create the session
it needs (bootstrapping is not desktop control). Everything that touches or
observes a running session stays deny-by-default, including `get_view` and
`stop_view`.

**`list_capabilities`** (read-only, always-on): what the caller's scope may
call (grantable categories/tools it holds), plus its effective quotas and
audit flags. A fresh agent can discover what *could* be granted without string
matching error prose.

**Names are additive-only** (agents cache them; A7). Plain snake_case; no theme
words (02). Core names pinned here: the lifecycle set above, `get_view`,
`stop_view`, `screenshot`, `clip_video`, `list_history` (grantable; same
scoping rule as sessions), plus `clip_audio` when built. Remaining
control-tool names are pinned as each category lands; they may only be added,
never renamed.

**First landing category pinned (input, nine tools):** `pointer_move {x, y}`
(absolute; session output pixels — the same coordinate space as `screenshot`
output), `pointer_click {button: left|right|middle}` (down+up at the current
pointer position), `pointer_button_down {button}`, `pointer_button_up {button}`
(the drag primitive: down → move → up), `pointer_scroll {dx, dy}` (wheel
lines; positive x scrolls right, positive y scrolls down), `key_tap {key}`,
`key_down {key}`, `key_up {key}` (xkb keysym names; chords are modifier
key_down/key_up around the tap), `type_text {text}`.

**Starter essential set to build first:** window management, input, screen
capture (non-optional — it closes the observe-then-act loop), query, program
launch. Decorations, animation curves, monitor arrangement, dynamic
multi-monitor/resolution changes: later expansion, non-blocking.

## Views over MCP

- **`get_view`** (control manifest, deny-by-default): session-scoped,
  idempotent — starts a view if none is running (same on-demand mechanism as
  `--view`, PDEATHSIG child), returns its address either way. A discrete MCP
  call hands off to a standing stream: the realtime consumer then talks to the
  returned address directly and never touches MCP again for perception (05
  keeps this seam; continuous streams are not tool calls).
- **`stop_view`** (control manifest): stops the view only. Session unaffected
  (06). With no view running it is idempotent success; on a dead session the
  view is already gone (`session_dead`). Start failure (wayvnc unusable,
  ports exhausted, front bind refused) ⇒ `view_unavailable`.
- The returned address is the manager-owned front (07), carrying its per-view
  credential (07). Reachability is explicit: the front binds loopback by
  default; when `view.advertise_base` is set (08), returned addresses are
  built from it for consumers outside the manager's namespace — **v1 does not
  provide zero-config remote viewing on the shared shape**: remote viewing
  requires explicit operator exposure (publish the view range, 09); views are
  not relayed in v1 — the dashboard-phase relay remains the provisioned
  future path.
- Audio has no standing stream yet (named v1 cut, 01). If built, same shape.

## Capture, media, and the content-type rule

Visual/audio data uses MCP's standard content types — no custom datatype:

- `screenshot` → an `image` content block (base64 + `mimeType`) in the result.
- `clip_audio` (later) → an `audio` block, same way.
- `clip_video` → a **resource link** to a manager-served media endpoint. The
  MCP spec has no video content type; a fetchable URI is the spec's own shape
  for artifacts too large to inline.

**Media endpoint (A8)**: the manager mints the `clip_video` resource_link as a
single-use, TTL-bounded capability URL on the surface that served the call;
for network-bridge calls the bridge relays `/media/*` pass-through on its
existing listener (no validation there — the 128-bit opaque id **is** the
credential; ids are never written to history or logs). Lifetime, precisely:
deleted after the response body completes (an open fd keeps serving concurrent
readers; first-complete-wins, no Range/resume); later fetches get `404` with
`media_expired`. TTL default `media.ttl = 600` s. When a scope's audit config
enables persistence (`audit.store_clips`, `audit.store_screenshots`), the same
already-captured artifact is additionally persisted to the data dir — never a
second capture — and persistence never resurrects the URL (`<data>/media/`,
retention in 08).

## Errors (machine-usable, A6)

Every failure is a stable envelope, identical over MCP and RPC:

```
{ "code": "<stable_code>", "message": "<human text, plain>", "hint": "<next action, optional>" }
```

MCP tool errors carry this envelope 1:1; **codes are a versioned API surface**
(additive-only, like tool names). Canonical codes:

- Session: `session_not_found`, `session_reaped`, `session_closed`, `session_dead`
  (reaped ≠ closed ≠ dead — distinct codes, distinct states, 06)
- Identity: `no_session_specified`, `scope_unknown`, `scope_auth_required`, `scope_required`, `token_required`, `no_manager_running`
- Permissions/limits: `tool_not_granted`, `quota_exceeded` (message carries scope + global numbers), `rate_limited`, `draining`
- Creation: `renderer_refused`, `backend_busy` (host exclusivity, 03), `limits_unavailable` (containment preconditions, 06 — fails manager start)
- Protocol: `version_incompatible` (message carries both version pairs), `bad_request`, `internal`
- Views/media: `view_unavailable`, `media_expired`
- Host/deps: `bind_refused`, `hyprland_missing`, `hyprland_unsupported`, `module_not_installed` (messages = the 04 strings, verbatim)
- Transport (client-side presentation): `manager_unreachable` — the shared string in 04.

`hint` is the corrective step where one exists (e.g. "create a session first",
"close a session or raise the scope quota"). `bad_request` means a malformed
request only (schema, grammar, unknown keys, unknown cursor); semantic
refusals always carry their own code.

## Pinned read shapes

- Session object: `{id, label, state, backend, resolution, scale, scope,
  origin, idle_since, view: {state, address, generation} | null, instance_id,
  renderer}`.
- Instance status: `{state, mode, pid, socket, versions: {binary, protocol,
  min_supported, dashboard_module?}, sessions: {running, dead}, drain:
  {active, remaining}, instance_id}`.
- List page: `{sessions: [...], next_cursor}`.
- MCP carriage: tool errors use MCP tool-execution errors (`isError: true`)
  with the envelope in `structuredContent`; JSON-RPC protocol errors are
  reserved for malformed MCP requests.

## List, history, and retries

- **Pagination**: `list_sessions`, history reads, and `list_capabilities`
  accept a `cursor` and return `next_cursor` — no unbounded result sets.
  Cursors are opaque, valid across registry changes (deletions shorten pages),
  and expire with the manager; an invalid cursor is `bad_request`.
- **Idempotency**: `create_session` and control calls accept an
  `idempotency_key` — string ≤ 64 chars, unique per (scope, logical
  operation). Dedup state is manager-lifetime, in-memory, lost on restart:
  after a restart the same key may re-execute (the original session died with
  its manager; create results carry `instance_id`, so this is visible, never
  silent). The create window runs from receipt until the created session
  ends. Documented retry rule: on `manager_unreachable` or a timeout, a client
  may retry **once** with the same key; without a key, retries are
  at-least-once and may duplicate (the reason the key exists — flaky
  connections are explicitly in the design, 03).

## Version handshake (A7)

- **The handshake call.** Every manager connection opens with the `handshake`
  call — always first; tokenless on every transport, deliberately pre-auth (a
  routing aid that returns versions + `instance_id` and authorizes nothing;
  the trust decision remains the credential presented after it).
  `{protocol_version, min_supported}` in; `{protocol_version, min_supported,
  instance_id}` out. `version`, `status --json`, and readiness surface
  `min_supported`. Edge cases, pinned: omitted `min_supported` is treated as
  equal to `protocol_version`; inverted or empty ranges are
  `version_incompatible`.
- **Comparison rule**: a connection is allowed iff
  `max(client.min_supported, manager.min_supported) <= min(client.protocol_version, manager.protocol_version)`
  — i.e. the supported ranges must overlap. Otherwise: `version_incompatible`,
  message carries both pairs (string in 04).
- Protocol version is its own semver, separate from the binary version.
- **The server cannot detect a newer client's unsupported call** under
  additive-only growth — only the client can pre-check. Clients pre-check
  before calling anything outside their known surface; managers reject
  out-of-range handshakes at connect.
- Additive-only applies to the RPC surface, error codes, and tool names.
- The dashboard never rejects a manager on version: it degrades per-manager
  via capability detection (10); a small compat table ships with release notes
  (09).
- Readiness: the `readiness` call — the RPC form of `status --json` (the same
  field set plus `dashboard_module`) — is the thing operators and the
  dashboard watch (C3). One contract, stated once.
