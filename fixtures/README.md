# `fixtures/ipc/` — CLI conformance scenarios

One RON file per scenario, one `#[tokio::test]` per file. Each scenario runs the real
`daiorama` binary against a **fake manager** that serves a scripted step list, then asserts the
process's stdout, stderr and exit code. Nothing here spawns a compositor, touches cgroups or
opens wayvnc: the fake manager speaks the frame and nothing else, which is what makes these
tests the contract between the CLI half and the runtime half (W2) of P2.

```sh
cargo test --test fixtures                       # every scenario
cargo test --test fixtures -- close_unknown      # one scenario
```

The suite fails if a file in this directory is not wired into `tests/fixtures.rs` — a fixture
that silently never runs is worse than no fixture.

## Scenario schema

```ron
(
  name: "close-unknown",                      // must equal the file stem
  description: "one line of intent",          // shown in failure output

  files: {                                    // optional: seeded into the temp instance dir
    "config.ron": "(version: 1)",
    "scopes/operator.ron": "(sessions: (visibility: all))",
  },

  remote: Some("$socket"),                    // optional; this is the default (see below)
  start: Some((exit_code: 1, stderr: "...")), // optional; presence = a lazy start is expected

  steps: [                                    // scripted exchanges, strict order
    (
      expect: (method: "handshake", params: (protocol_version: "0.1.0", min_supported: "0.1.0")),
      respond: (result: (protocol_version: "0.1.0", min_supported: "0.1.0", instance_id: "…")),
    ),
    (
      expect: (method: "close_session", params: (session_id: "missing")),
      respond: (error: (code: "session_not_found", message: "session not found", hint: "list sessions to find one")),
    ),
  ],

  runs: [                                     // each entry is one process invocation
    (
      argv: ["close", "missing"],
      expect_exit: 1,
      expect_stderr_contains: ["session not found (session_not_found)"],
    ),
  ],
)
```

### `name`, `description`

The name is the file stem and is asserted against it. The description is one line of intent;
it appears in every assertion failure so a red test explains itself without opening the file.

### `files` (optional)

Seeded verbatim into the scenario's temp config dir:
`<XDG_CONFIG_HOME>/daiorama/<instance>/<key>` — so `"config.ron"` is the core config and
`"scopes/operator.ron"` is a scope file, exactly as 08 lays them out. Keys may nest with `/`.
The data dir (`<XDG_DATA_HOME>/daiorama/<instance>/`) is created empty for every scenario.

### `remote` (optional)

| value | effect |
|-------|--------|
| omitted, or `Some("$socket")` | the harness passes `--remote <fake manager socket>` before the subcommand |
| `Some("<literal>")` | that literal address is passed (with `$` tokens substituted) |
| `None` | **no** `--remote`: the command runs against the local instance |

`None` is what the cold-answer and lazy-start scenarios need: with `--remote` the CLI never
starts anything locally and an unreachable manager is an error (04 B5), so "no manager running"
and "start a manager" can only be exercised without it.

### `start` (optional)

The lazy-start stub: `(exit_code: N, stdout: "...", stderr: "...")`. The harness points
`DAIORAMA_MANAGER_BIN` at a generated POSIX-sh script that appends to a marker file and then
emits exactly that output.

- **`start` present** = this scenario expects a lazy start: the marker must exist afterwards.
- **`start` absent** = no start is expected: the stub still exists (so an *unexpected* start is
  loud rather than silent) and the marker must NOT exist afterwards.

A started manager reports a startup failure as **one line of the frame's error form on
stderr**, e.g. `{"error":{"code":"limits_unavailable","message":"…","hint":"…"}}`; the launcher
surfaces that envelope verbatim. `manager-start-limits-unavailable.ron` and
`manager-start-limits-off-mode.ron` pin the two containment outcomes through this channel.

### `steps`

The scripted exchanges. The fake manager binds `<runtime dir>/daiorama/<instance>/manager.sock`
whenever there is at least one step, and consumes the list **in order across every connection
and every run**.

- **Handshake first, always.** The first frame on every connection must be a `handshake` step;
  anything else is a failure. Every run is a fresh process, so every run needs its own
  handshake step — declare them explicitly, in run order.
- **Frames are bounded.** One frame — the JSON text between newlines — is at most 8 MiB
  (`MAX_FRAME_BYTES`). A longer line is refused with `bad_request` (`request frame must be at
  most 8388608 bytes`) and the connection ends — nothing follows the refusal, like a refused
  handshake. The cap is exact: a frame at the limit is read normally.
- **Lines that cannot be frames are refused, and the connection ends.** A line whose bytes are
  not valid UTF-8 is refused with `bad_request` (`request line is not valid UTF-8`); a second
  `handshake` on one connection is refused with `bad_request` (`handshake may only be the
  first call on a connection`). Nothing follows either refusal, like a refused handshake.
- **Dispatch refusals answer by class, and do not end the connection.** A method name outside
  the pinned surface is refused with `bad_request` (`unknown method "foo"`); a pinned-surface
  name this build does not implement yet — the views/query methods, `daemon_end`, the control
  tools — answers `internal` (`not implemented yet`). A method whose pinned surface takes no
  parameters (`status`, `readiness`, `config_reload`, `drain`) refuses any it is given with
  `bad_request` (`unknown parameter "bogus"; this method takes no parameters`). All are
  dispatch-level refusals: unlike the frame- and gate-level refusals above, the connection
  continues. A dispatch that panics answers `internal` (`dispatch panicked`) and the
  connection continues — dispatch runs on the blocking pool, so a panic cannot unwind into
  the reactor and must not read as a dropped connection.
- **Scope and origin ride the frame, not the params.** A request may carry `scope` (absent =
  `operator`) and `origin` (absent = `shell`); an `origin` outside `shell` | `mcp` is refused
  with `bad_request` (`invalid origin "x"; expected one of: shell, mcp`). The MCP bridge's
  tool surface uses the same class: a tool name outside the registry is `bad_request`
  (`unknown tool "x"`), a known but ungranted name is `tool_not_granted`.
  The bridge's stdin frames follow the same rule: an absent `id` is a notification (no
  response, nothing executed) while `"id": null` is an invalid request answered `-32600`
  (`id must be a string or a number, not null`) and the connection continues.
- **On the TCP manager transport (`manager.bind`), credentials ride the same frame.** Every
  request after the tokenless handshake carries `token` (the instance token) and, with
  `scope`, `scope_token` (the scope's secret). A missing or incorrect `token` answers
  `token_required` and the connection ends. A missing `scope`/`scope_token`, a secretless
  scope or a wrong secret answers `scope_auth_required` — `scope required: set scope and
  scope_token on the request frame`, `scope "x" has no secret; it cannot be used over the
  network`, `scope "x" requires its scope secret (scope_token frame field)` — and the
  connection continues; a `scope` outside `[a-z0-9][a-z0-9_-]{0,63}` is `bad_request`
  (`invalid scope name`) and the connection continues. The unix socket never carries either
  field.
- `expect.method` must match exactly. Methods mirror MCP tool names where they exist
  (`create_session`, `close_session`, `list_sessions`, `session_status`, `get_view`,
  `stop_view`, `list_history`, and the control tools by their own names) and are RPC-only
  otherwise (`handshake`, `status`, `readiness`, `config_get`, `config_set`, `config_reload`,
  `drain`, `daemon_end`).
- `expect.params` **omitted (or `()`) means "no params": the request must carry `params: null`**.
  Present, it is a *subset* match: every field in the fixture must match, extra keys in the live
  request are allowed (the CLI may add defaults). Numbers compare numerically, so `1.0` and `1`
  are equal.
- `idempotency_key` is frame-level and is **not** asserted by the runner today.
- `respond` is exactly one of `(result: …)` or `(error: (code: …, message: …, hint: …?))` — the
  frame's response shape. Codes must look like canonical codes (lowercase snake case).

A request with no step left, a wrong method, a params mismatch, or a connection that opens with
something other than `handshake` fails the scenario, and the message says what the fake manager
saw. Runs are asserted first: a failing run reports the fake manager's account of the traffic as
well, so one failure is usually enough to diagnose.

### `runs`

Each entry is one `daiorama` invocation, with `--instance <instance>` and (unless `remote:
None`) `--remote <addr>` passed before `argv`.

| field | meaning |
|-------|---------|
| `argv` | arguments, exactly as an operator would type them after `daiorama` |
| `expect_exit` | expected exit code; default `0`. `0` = an answer (including the cold `no manager running` answer), `1` = an error envelope |
| `expect_stdout_contains` / `expect_stdout_not_contains` | substring assertions on stdout |
| `expect_stderr_contains` / `expect_stderr_not_contains` | substring assertions on stderr |
| `expect_stdout_json` | **exact** equality against the parsed stdout document (after token substitution) |

`--json` prints the response payload unmodified on stdout (the pinned 05 read shape), and
errors as the `{code, message, hint?}` envelope verbatim on **stderr** — which is why the
`--json` error assertions look at stderr and the cold answer's assertion looks at stdout.

### Tokens

Every fixture string and every expectation is substituted before use:

| token | value |
|-------|-------|
| `$socket` | the fake manager's socket path in this scenario's temp root |
| `$manager_pid` | the pid of the process running the fake manager |
| `$config_dir`, `$data_dir`, `$runtime_dir` | this scenario's temp directories |

That is what lets `status-instance-json` assert a pid and a socket path it cannot know when it
is written.

### Environment the CLI sees

`HOME`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_RUNTIME_DIR` and `TMPDIR` all point into the
scenario's temp root; `DAIORAMA_TOKEN`, `DAIORAMA_SCOPE` and `DAIORAMA_SCOPE_TOKEN` are cleared;
`DAIORAMA_MANAGER_BIN` points at the start stub. Nothing outside the temp root is read or
written, and scenario sockets live under `/tmp` so unix path-length limits are never in play.

## RON notes for fixture authors

- `()` is JSON `null` — RON has no null literal for value bodies. `view: ()` is `"view": null`.
- Optional fields in the *fixture schema* use RON's option syntax: `Some(...)` / `None`.
  Inside value bodies there is no `None`: omit the key instead.
- `//` and `/* */` comments are welcome; these files are meant to be readable.

## What each scenario proves

| scenario | pinned by |
|----------|-----------|
| `handshake-ok` | 05 A7 — handshake is the first call on every connection, tokenless |
| `handshake-version-mismatch` | 05 A7 comparison rule; 04 mismatch string, both pairs, exit 1 |
| `new-session-ok` | 04 pinned creation defaults; 06 `instance_id` in create results; id printed |
| `new-refused-software-renderer` | 06 D6 renderer policy; 04 refusal string byte-for-byte |
| `new-host-busy` | 06 B6 host exclusivity; 04 string naming the incumbent session |
| `new-quota-exceeded` | 05 `quota_exceeded` carries scope + global numbers; 04 string |
| `list-paged` | 04 `next_cursor` on truncation; 05 list-page shape |
| `status-instance-json` | 05 instance-status shape, full field set |
| `status-session-json` | 05 session object, `view` null and non-null (with generation) |
| `control-unknown-action` | 04 `bad_request` listing valid names; control args typed by inputSchema |
| `close-closed` | 06 closed is final and distinct: `session_closed` |
| `close-unknown` | 05 `session_not_found` with the corrective hint |
| `session-reaped` | 06 reaped ≠ closed ≠ dead: `session_reaped` |
| `tombstone-expired` | 06 tombstones are bounded: past the window the id is `session_not_found` |
| `state-transitions` | 06 B3 `running → degraded → dead` as the CLI renders it |
| `dead-sessions-separate` | 05 `sessions: {running, dead}`; dead entries stay inspectable |
| `session-dead-call` | 06 calls on a dead session: `session_dead`; closing one is allowed |
| `view-down-session-running` | 06 view down ≠ session dead; generation moves on restart |
| `compositor-death-no-restart` | 06 crash policy: dead, listable, `cause=dead` in history, no auto-restart |
| `cold-no-manager` | 04 cold class: an answer, exit 0, and nothing starts |
| `config-getset-ok` | 08 A2: manager-side keys read/written over RPC; client-local keys refused on a remote |
| `remote-unreachable` | 04 shared string; `--remote` never falls back locally |
| `manager-start-limits-unavailable` | 06 C6 startup refusal, 04 containment string |
| `manager-start-limits-off-mode` | 06 C6 explicit downgrade: the start proceeds |
| `frame-verbs` | 04 `drain` in all forms (enter, `--wait`, `--json`, the `--timeout` bound), `config reload` rendering, `view --stop` |

Provisional shapes (mine to pin, additive-only afterwards, flagged in the P2 report): the
history read shape (`{records: [{at, event, session_id, scope, origin, cause?}], next_cursor}`),
the human renderings, and the `bad_request` messages that 04/05 do not spell out.

## Network bridge (`daiorama mcp --listen`, P3 W1b)

The network mode serves the same MCP surface on one HTTP endpoint: **`POST /mcp`**, JSON-RPC
body, `Content-Length` (no chunked bodies), keep-alive. `GET`/any other path is refused. `Host`
is required, and an `Origin` header (when present) must name the same authority as `Host`.
Bodies are bounded exactly like a stdio frame — `MAX_FRAME_BYTES` (8 MiB), over-cap refused
with the same `bad_request` string and the connection ends.

Every request authenticates itself with the pinned headers (05/07): `Daiorama-Token` (the
instance token, checked first), `Daiorama-Scope`, `Daiorama-Scope-Token` (that scope's secret).
No anonymous calls, no default scope. A scope is network-usable only while it has a secret file
(`<data root>/scope-tokens/<scope>`, 0600 — 08). MCP-level errors ride the 200 envelope only
when the request id is known; an id-less refusal answers its HTTP status with the same envelope
and `Connection: close` — `403` for the identity gate (the three refusals below), `400` for a
parse error, `413` for a declared body over `MAX_FRAME_BYTES`. Never an HTTP auth challenge or
discovery metadata, and never `401` (the OAuth-discovery trigger). A notification (no `id`) passes the gate, executes nothing,
and answers `202` with an empty body.

Refusal strings (pinned; `{scope}` and `{origin}` are the observed values, quoted as JSON):

- `scope required: send Daiorama-Scope and Daiorama-Scope-Token headers` — `scope_auth_required`
- `scope {scope} requires its scope secret (Daiorama-Scope-Token header)` — `scope_auth_required`
- `scope {scope} has no secret; it cannot be used over the network` — `scope_auth_required`
- `origin not allowed: {origin}` — `bad_request`
- `network bridge serves POST /mcp only` — `bad_request`
- `network mode serves the local instance; use stdio for a remote manager (--remote)` — `bad_request`
- `missing Host header` — `bad_request`
- `chunked request bodies are not supported` — `bad_request`
- `request head too large` — `bad_request`
- `malformed HTTP request` — `bad_request`
- `duplicate Content-Length` — `bad_request`
- `invalid Content-Length: {raw}` — `bad_request`

A head is parsed strictly, because a `Content-Length` body means two readers of the same bytes
must agree on where the next request starts. Refused as `malformed HTTP request` (400, the
connection ends): more than one `Host`, an empty `Host`, whitespace between a field name and its
colon (a folded continuation line included). Refused on their own strings: a repeated
`Content-Length` (`duplicate Content-Length`) and a `Content-Length` that is not `1*DIGIT` — a
leading `+`, an empty value, or any non-digit (`invalid Content-Length: {raw}`).

A wrong or missing `Daiorama-Token` answers `token_required` with the 04 client string (the
gate does not distinguish missing from wrong); an unknown scope answers `scope_unknown`.

### Media pass-through (`GET /media/<id>`)

The network listener serves one media target beside `POST /mcp` (05 A8): `GET /media/<32
lowercase hex>` is relayed to the manager's media surface and streamed back. The 128-bit id
**is** the capability; this listener's gate is the **instance token only** (`Daiorama-Token`)
— scope headers are not part of it, and a plain HTTP client with the token can fetch. The
request target is never logged.

- `405` + `Allow: GET` — any method other than GET (`media fetches are GET only`).
- `404` + `media_expired` — a path that is not exactly `/media/<32 lowercase hex>` (uppercase,
  wrong length, separators, traversal shapes; `/media` and `/media/` included), refused at the
  bridge without contacting the manager.
- `403` + `token_required` — missing or wrong instance token.
- `502` + `manager_unreachable` — the manager is down.
- `502` + `internal` (`media upstream answered without a Content-Length`) — a malformed
  upstream answer (no length, or chunked); the bridge does not guess where the body ends.
- A `404` from the manager passes through with its `media_expired` body. `Range` is not
  supported: a ranged request receives the full `200` body.
- Every media refusal closes like every other refusal on this listener — response, half-close,
  then a bounded drain when the request carried no `Expect` — so a body already in flight does
  not reset the refusal away (`p3/media/evidence/probe-media-stall.py`).
- A client that accepts no bytes for 10 s is cut off mid-stream; a slow reader that keeps
  taking bytes is not (the bound is per write, not a total-transfer deadline).

### Capture (05 §Capture)

- `screenshot` and `clip_video` run the configured argv command under the session's own
  Wayland environment; the manager appends the output path and interpolates nothing else.
- `screenshot` is size-checked **raw** before base64: over `capture.screenshot_max_bytes`
  answers `bad_request` `capture too large: {bytes} bytes (max {max})` — never a downscale.
- A failed command answers `capture_failed` with `capture failed: {command} exited {code}` or
  `capture failed: {command} timed out after {n}s`; empty or non-PNG output is
  `capture failed: no output` / `capture failed: not a PNG`.
- `clip_video` answers with a minted 32-hex id; the network bridge builds
  `http://<Host>/media/<id>`, stdio builds `daiorama://media/<id>` and names
  `daiorama media fetch <id> --out <path>` (claim-on-open applies to both).

### History (05, 06 B8, 08)

- History pages answer `{records: [...], next_cursor: <opaque> | null}`; each record is
  `{at, event, session_id, scope, origin, cause?, detail?}`.
- Control-call arguments are never recorded; neither are media ids or tokens.
- Session dirs carry a `.ended` sidecar (RFC 3339 + cause) — the retention sweep's input; 8
  segments × 4 MiB rotate inside a session, oldest dropped.

### Rate limit and capture (05 §Rate, 07 B9)

- Control-class input calls are rate-limited per session by `limits.rate`; reads, capture and
  lifecycle are exempt. `pace` waits in arrival order; a wait beyond `max_wait_ms` or a queue
  beyond `max_queued` answers `rate_limited` with `retry after <ms> ms`.
- One capture at a time per session: a concurrent call answers `bad_request` with
  `a capture is already running for this session`.

### Views (05/07)

- The front serves only the noVNC asset tree and `/websockify`; a missing tree is `503` +
  `view_unavailable` with `view assets not installed (expected <data>/novnc)`.
- Every front response carries `Referrer-Policy: no-referrer`; the `Origin` header is required
  on the WebSocket upgrade; the 07 credential is accepted as a query parameter,
  `X-Daiorama-View` header, or the `HttpOnly; SameSite=Strict` cookie, and never appears in
  logs or the session object.

### Media store and endpoint (manager side)

The manager serves the same `GET /media/<32 lowercase hex>` on its own socket, distinguished
from JSON-RPC frames by the first line (an HTTP request line) and answering one fetch per
connection with `Connection: close`. Unknown, expired and consumed ids are a single `404` +
`media_expired`; a non-GET is `405` + `Allow: GET`; a declared body is `400`
(`media fetches carry no body`); an over-cap head keeps `request head too large`. The store is memory-only for
the URL lifetime — a manager restart drops every id — and `audit.store_clips` persists the same
artifact at `<data>/media/<session>/<id>` (0600, per-session cap, oldest-first eviction), never
resurrecting the URL.

## Spawner parameters

What the fixtures pin is the **frame** contract: `create_session` carries the requested
`resolution` (logical) and `scale` (04 defaults `1920x1080` / `1.0`), plus `backend`, `view`
and an optional `label`.

What the spawner does with that pair — the intended arithmetic is `mode = logical × scale`
(physical) — belongs to the spawner and no fixture asserts it: these scenarios pin the
parameters on the wire, not what the compositor does with them. When the spawner lands, its own
tests carry that arithmetic.

## Containment classes (06 C6)

Three classes, one decision function (`manager::startup::plan_startup`); raw evidence for the
container shapes lives in `p2/cgroup-matrix/`:

1. **Unavailable → off-mode class** — the default in containers: the cgroupfs is read-only or
   delegation is absent, so `limits.mode = on` fails the start with `limits_unavailable`, and
   the supported shape is the explicit downgrade `limits.mode = off`: no per-session cgroups, no
   crash-time `cgroup.kill`, cleanup reduces to the parent-death signal, and **no startup sweep**
   (there is no session-cgroup subtree to sweep). `manager-start-limits-off-mode.ron` pins that
   the start proceeds in this class; `manager-start-limits-unavailable.ron` pins the refusal in
   the on-mode class; `manager::startup`'s unit tests pin the plan itself (limits disabled,
   `sweep: false`).
2. **On, containerized** — privileged (or equivalently delegated) containers need keeper prep
   before controllers can be used: empty the `nsdelegate` namespace root, *then* enable the
   controllers — enabling them while the root cgroup still has processes fails with `EBUSY`.
   The exact sequence and its raw output live with the containment work.
3. **On, host-process manager** — delegation is present (a delegated scope such as
   `systemd-run --user … -p Delegate=yes`): the probe succeeds, limits are enabled, and the
   startup sweep runs over the instance's session-cgroup subtree to kill session trees left by a
   previous manager run (03 ownership seams).

`daiorama doctor` runs the same check via the in-tree probe and reports the class it finds;
the exact wiring command for that class stays the runtime side's output (W4).
