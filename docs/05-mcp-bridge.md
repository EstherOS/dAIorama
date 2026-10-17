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
  connections, one HTTP endpoint (`POST /mcp`). Scope is authenticated **per
  request** from the pinned credential headers — a connection carries no
  identity of its own, so a pooled client can multiplex scopes safely; no
  anonymous calls; no default scope. Scopes without a secret are usable via
  stdio only (07 states the model; rationale in 11). Network mode serves the
  local instance; a remote manager stays the stdio bridge's shape (`--remote`).
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
  (per-request `_meta["io.modelcontextprotocol/protocolVersion"]` declaration;
  `server/discover`) and the legacy
  `initialize` negotiation for earlier revisions; on stdio it probes with
  `server/discover` first. `UnsupportedProtocolVersionError` (-32022) maps to
  `version_incompatible` with both pairs in the message.

**Stdio conformance surface (P3 W1).** The bridge answers the handshake paths and the
manifest locally, and relays everything else:

- `server/discover` answers the modern result shape
  `{supportedVersions: ["2026-07-28"], capabilities: {tools: {listChanged: false}},
  instructions: <one line>}`, with the identity on the display-only
  `_meta["io.modelcontextprotocol/serverInfo"]` stamp. `initialize` (the handshake-era
  path) answers `{protocolVersion: <the revision the client named when it is a
  handshake revision (2024-11-05 · 2025-03-26 · 2025-06-18 · 2025-11-25), else
  "2026-07-28">, capabilities: {tools: {listChanged: false}}, serverInfo: {name:
  "daiorama", version: <binary version>}, instructions: <one line>}` — a server names a
  version it supports, and the tool surface is revision-stable. Neither authorizes
  anything.
- `tools/list` answers `{tools: [{name, description, inputSchema}]}` — exactly the
  caller scope's exposed manifest (the always-on set plus grants), never the whole
  registry. A 2026-07-28 request gets the modern result envelope on every answer:
  `resultType: "complete"` on each result, plus `ttlMs: 0` (never stale — a config
  reload is visible on the next call) and `cacheScope: "private"` on the cacheable
  list result. A modern client validates these as required fields; a handshake-era
  request keeps the pre-2026 shape.
- `tools/call` gates the name first: a name outside the registry is `bad_request`
  (`unknown tool "<name>"`); a known name the scope does not hold is
  `tool_not_granted`; a granted name is relayed to the manager. Tool failures are
  MCP tool-execution results (`isError: true`, envelope in `structuredContent`);
  JSON-RPC errors are reserved for malformed MCP requests. Every result carries
  `content` (an empty list when there is nothing to say) — client SDKs validate the
  field as required.
- `list_capabilities` is answered from the scope file plus the manager's own
  filtered listing: `{tools, categories, always_on, quotas: {sessions: {max,
  live}}, audit}`, paginated over `tools` with `next_cursor` (cursor/limit per
  §List, history, and retries).
- A notification (no `id`) never executes and never gets a response.

**Network conformance surface (P3 W1).** `--listen` serves the same surface over HTTP on one
endpoint, `POST /mcp`; bodies are `Content-Length`-delimited and bounded by `MAX_FRAME_BYTES`
exactly like a stdio frame (over-cap: `bad_request`, connection ends). `Host` is required and
an `Origin` (when present) must name the same authority as `Host`. Each request carries its
own credentials (`Daiorama-Token`, `Daiorama-Scope`, `Daiorama-Scope-Token`); the gates run in
that order before dispatch. **MCP-level errors ride the 200 envelope only when the request id is
known.** A refusal that cannot name the id — the head-stage identity gate, a body parse error, a
declared-size refusal — answers its HTTP status with the same envelope and `Connection: close`:
`403` for identity-gate refusals (`token_required`, `scope_auth_required`, `scope_unknown`),
`400` for parse errors, `413` for a declared body over `MAX_FRAME_BYTES`. A request/response
client matches by id, so a 200 with `"id": null` is unmatchable and presents as a server hang;
the non-200 status is what makes the refusal visible. `401` is never used — it triggers OAuth
discovery. A notification that passes
the gate executes nothing and answers `202` with an empty body. The head is strict where a
fronting proxy is strict: more than one `Host`, an empty `Host`, whitespace between a field name
and its colon, a repeated `Content-Length`, or a value that is not `1*DIGIT` are all
`bad_request` and the connection ends (11). The refusal strings are pinned in
`fixtures/README.md`'s network contract.

**Media pass-through (`GET /media/<id>`).** The listener also serves the manager's media
surface: `GET /media/<32 lowercase hex>` is relayed to the manager and streamed back
unchunked. The 128-bit id is the capability (A8); this listener's gate is the **instance
token only** — the `/mcp` scope headers are not part of a media fetch (the asymmetry is
pinned here and in `fixtures/README.md`). Any other `/media` shape is `404` +
`media_expired` **at the bridge**, never an upstream request; any method other than GET is
`405` + `Allow: GET`; `Range` is not supported (a ranged request receives the full body).
An upstream answer without `Content-Length`, or one that is chunked, is `502` +
`media upstream answered without a Content-Length` — the bridge does not guess where a body
ends. The request target is never logged (A8). A media refusal closes like every other refusal
on this listener (response, half-close, bounded drain), so a request whose body is already in
flight does not lose the refusal to a reset. A client that accepts no bytes for the same 10 s
progress bound is cut off mid-stream; a slow reader that keeps taking bytes is not — the bound
is per write, not a total-transfer deadline. The manager side serves that `GET` on its own
socket (HTTP distinguished from JSON-RPC by the first line; one fetch per connection,
`Connection: close`); the store is memory-only for the URL lifetime — a restart drops every id —
and `audit.store_clips` persists the artifact beside its session (`<data>/media/<session>/<id>`,
08) without ever resurrecting the URL.

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

**Credentials on the TCP manager transport (`manager.bind`, 07).** The same
frame gains two additive credential fields, presented on every call after the
tokenless handshake:

- `token` — the instance token (the transport gate).
- `scope_token` — the scope's network secret, presented with `scope`. A
  claimed string is never authentication: a scope with no secret file cannot
  be used over the network.

The unix socket stays tokenless, ever: a local frame may omit both and their
presence is ignored there. Over TCP the gate class decides what follows a
refusal: a missing or incorrect `token` answers `token_required` and ends the
connection (transport gate — "nothing follows a refusal"); a missing
`scope`/`scope_token`, an unknown or secretless scope, or a wrong secret
answers `scope_auth_required` and the connection continues (scope identity is
per request, so a pooled connection can multiplex scopes). A `scope` that is
not a valid scope name — `[a-z0-9][a-z0-9_-]{0,63}`, 08 — is refused
`bad_request` (`invalid scope name`) before any path is joined, and the
connection continues. The media endpoint on a TCP manager surface requires the
instance token in a `Daiorama-Token` header (the id remains the artifact's own
capability; scope headers are not part of a media fetch).

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

The manager answers the two tools with a typed payload the bridge maps onto those
content types (never a custom datatype):

```
screenshot  → {"capture":"image","mimeType":"image/png","data_b64":…,"bytes":N,"width":W,"height":H}
clip_video  → {"capture":"media","media_id":"<32 hex>","mimeType":"video/webm","bytes":N,
               "duration_s":D,"expires_in_s":T}
```

`screenshot` size-checks the **raw PNG before base64** (`capture.screenshot_max_bytes`,
default 4 MiB — 4 MiB raw encodes to ~5.3 MiB, under the 8 MiB frame bound); over it is
`bad_request`, never a downscale or a truncation. `clip_video`'s link URI is built at the
bridge: the network listener uses its request's own `Host` (`http://<Host>/media/<id>`, so a
proxy or non-loopback bind works), and stdio returns `daiorama://media/<id>` plus one text
block naming `daiorama media fetch <id> --out <path>` (the local socket is the transport;
claim-on-open applies to the CLI exactly as to HTTP).

**Capture commands** are `capture.screenshot_command` / `capture.clip_command` (argv arrays —
never shell strings; the manager appends the output path and interpolates nothing else), run
under the **session's** `XDG_RUNTIME_DIR` / `WAYLAND_DISPLAY` / `HYPRLAND_INSTANCE_SIGNATURE`,
bounded by `capture.timeout_s`. A command failure is the `capture_failed` code; the harness
branches on the code, not the prose.

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
- Permissions/limits: `tool_not_granted`, `quota_exceeded` (message carries scope + global numbers), `rate_limited` (hint `retry after <ms> ms` — B9; control-class calls only, per session, paced or rejected per `limits.rate`), `draining`
- Creation: `renderer_refused`, `backend_busy` (host exclusivity, 03), `limits_unavailable` (containment preconditions, 06 — fails manager start)
- Protocol: `version_incompatible` (message carries both version pairs), `bad_request`, `internal`
- Views/media/capture: `view_unavailable`, `media_expired`, `capture_failed` (a capture command failed or produced nothing usable; message carries the command and reason)
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
- History page: `{records: [record], next_cursor: <opaque> | null}`; a record is
  `{at, event, session_id, scope, origin, cause?, detail?}` — control-call
  arguments are never recorded (06), and `at` is RFC 3339 UTC.
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
