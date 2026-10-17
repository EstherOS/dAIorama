# 07 — Network surfaces, tokens, scopes

This file is the complete enumeration of network-reachable surfaces. Every new
surface is added to this table when introduced — the previous design failed
exactly here (the flagship bridge shape was missing from the enumeration and
the view-port gate had no enforcer).

## The surfaces

| surface | default | when exposed beyond loopback |
|---------|---------|------------------------------|
| manager RPC | unix socket, zero-config, tokenless | TCP bind requires instance token, always; frame credentials below |
| MCP bridge, network mode | loopback (`127.0.0.1:8757`) | instance token **and** per-request scope auth (below) |
| view front (noVNC + proxy to wayvnc) | loopback | per-view credential derived from the instance token (HMAC, below), enforced by the front itself |
| dashboard | loopback | **v1 refuses non-loopback binds** (remote humans use SSH forwarding or an overlay network); its manager connections carry per-connection tokens |
| media endpoint (`/media/<id>`) | on the manager surface; the bridge relays `/media/*` pass-through for network-bridge calls (instance token only at the listener; scope headers are not part of a media fetch) | the 128-bit opaque id **is** the credential (capability URL); ids never logged |
| unix socket | filesystem permissions only | — (stays tokenless, ever) |

## The exposure rule (one, everywhere)

- **The gate is request-time, not bind-time.** Bind-address checks are fragile
  the moment anything proxies or publishes a port; the component in front
  enforces per request instead. The unix socket stays tokenless — the same
  reasoning as `/var/run/docker.sock`.
- **Any network bind requires the instance token configured.** It is
  generated automatically on the first network listen bind when none exists —
  loopback included: a network listener never runs tokenless (only the unix
  socket is tokenless). If it cannot be generated (read-only location), the
  start refuses loud — string in 04. Never a silently open port.
- **The view port has a real gate for well-behaved callers (A3).** The
  manager-owned **view front** serves the noVNC static page, checks the
  per-view credential below, and proxies to a loopback wayvnc. Confirmed in
  the field: wayvnc speaks RFB-over-WebSocket only and never serves the noVNC
  page, so a companion server is required regardless — the gate belongs in
  front of both.
- **View credential (pinned).** The front mints a per-view bearer credential:
  `credential = HMAC-SHA256(instance_token, "view:" || session_id || ":" ||
  generation)`. Multi-use; valid only for the current generation (dies with
  stop/death structurally); revoked on `stop_view`. `view`/`get_view` return
  the front URL with the credential as a query parameter (and as a separate
  `credential` field in JSON output); session detail/status never echo it. The
  front gates **every** request — page, assets, and the WebSocket upgrade
  (which validates `Origin`) — and on the first credentialed page load sets
  its own HttpOnly session cookie scoped to the front origin so the noVNC
  page's asset/WS traffic is covered. The served page's WS target is pinned
  server-side (client-supplied host/port/path params ignored). One-time codes
  are dropped from v1 — multi-consumer reality (dashboard + programmatic
  consumer + human) is the norm this flow must serve.
- **Browsers are validated.** All manager-adjacent HTTP surfaces (the bridge's
  MCP endpoint, the view front, the dashboard) validate `Origin`/`Host` and
  bind loopback by default.
- **TLS is never dAIorama's job** (01 non-goal). Documented pattern: overlay
  network for encryption + identity (Tailscale/WireGuard family), and/or a
  mature reverse proxy for TLS termination, for anyone exposing further.

## Manager RPC over TCP (`manager.bind`)

`manager.bind` (08; restart-only) adds a TCP listener beside the unix socket.
The socket always stays, and the TCP surface always carries the instance
token — loopback included, because the gate is request-time, not bind-time
(above). `manager.bind` is a host or `host:port`; a bare host uses the pinned
manager TCP port (04). IPv6 literals use brackets (`[::1]:8756`).

- **Carriage.** The frame's `token`, `scope` and `scope_token` fields (05): the
  handshake stays tokenless and first; every call after it presents the
  instance token and its scope's secret. `scope` is required on this transport
  — there is no default scope on a network surface; the CLI names `operator`
  and presents its secret (`DAIORAMA_SCOPE_TOKEN`, 04).
- **Startup credentials.** A network start never runs tokenless: the manager
  ensures the instance token and a secret for every scope file present,
  minting what is missing and printing each path once (never a value). The
  token is created only when absent, so two racing starters converge on one
  value instead of the loser clobbering the winner's. The bound address is
  reported (`listening on <addr>`); a bind failure refuses the start — never a
  silent unix-only manager.
- **Refusals.** Missing or incorrect token: `token_required`, connection ends
  (transport gate). Scope: `scope_auth_required` (missing secret, secretless scope, wrong
  secret) and `bad_request` (`invalid scope name`, before any path is joined); both are
  per-request and the connection continues. The scope secret is checked before the scope
  file is read, so a scope's existence is never leaked to an unauthenticated caller.
- **Rotation window.** The token is adopted at manager start: `token rotate`
  rewrites the file at once, but the running manager keeps accepting the old
  value and refuses the new one until the next start. This surface is what
  makes that window enforceable on the wire (live test, no reload path).
- **Media.** A fetch on the TCP surface presents `Daiorama-Token`; the
  artifact id remains its own capability, and scope headers are not part of a
  media fetch.

## Instance token lifecycle (A5 — one rule, no contradiction)

- **Generated on the first network listen bind** if none exists; written to
  `<data>/<instance>/token` (0600, its own file, never in ordinary config);
  at that startup the path is printed **once**, with the reminder to run
  `daiorama token show` — the value itself is never written to logs; it is
  not re-printable anywhere except by `token show`.
- **The previous contradiction is resolved:** the manager does not "generate on
  first use" *and* "refuse without one" — it generates when it can, and refuses
  (04 string) only when generation is impossible while a non-loopback bind was
  requested.
- **Client side:** supplied via `DAIORAMA_TOKEN` (exact name) alongside
  `--remote`, never as a CLI argument (shell history, process listings).
  Missing-token client error: 04 string.
- **Scope is separate and finer:** this token is the coarse network gate;
  scope identity applies after it, not instead of it.
- **Rotation (A5):** `token rotate` regenerates the file and states plainly
  that the new token takes effect at next manager start and that restarting
  drops every running session (PDEATHSIG — there is no running state to
  migrate, and the previous "no running-state to migrate" note missed exactly
  this). Live token reload is not in v1; deliverable alternative recorded in 11.

## Scope model (A4 — decided: authenticated over network, cooperative on unix)

Scope is the application-layer identity carried on **every** call; the manager
enforces quotas and tool manifests against it regardless of transport.

- **stdio bridge:** scope set once via `DAIORAMA_SCOPE` (+ `DAIORAMA_SCOPE_TOKEN`
  when the scope has a secret) in the harness's own MCP client config. Process
  environment is the authentication; nobody else can set it for that process.
- **network bridge:** per-request authentication — a network MCP endpoint
  carries credentials as request headers: `Daiorama-Token` (instance token),
  `Daiorama-Scope`, `Daiorama-Scope-Token`; the bridge relays them unmodified
  and the manager validates on every call — the same presentation path as the
  instance token, for both hops. The manager's TCP RPC surface accepts the
  same credential presentation (same names; carriage per transport). **A
  claimed string is never authentication**; a
  scope without a secret cannot be used over the network.
- **Scope secrets (pinned):** one file per scope at `<data root>/scope-tokens/<scope>`
  (mode 0600, filename = scope name; 08). Presence is what makes a scope network-usable:
  a scope with no secret file answers `scope_auth_required` on the network and stays
  stdio-only. `config setup` mints the starter scope's secret; `daiorama mcp --listen`
  mints any scope file's missing secret at startup (announced on stderr, written 0600), so
  the first network start cannot leave a silent dead end. The instance token is likewise
  ensured at `--listen` startup, for loopback binds too — a network surface never runs
  tokenless.
- **local unix socket:** scope claims are cooperative and documented as such.
  The socket is filesystem-gated, and anything with code execution on the same
  host can bypass config-level restrictions anyway (01, principle 5) — local
  scope authentication would be theater. The shared/central shape, where
  differently-trusted callers come from different boxes, is where scope auth
  is real, and that is the network path.
- **Reserved scope `operator`:** sessions created from the CLI carry it; CLI
  reads are operator-wide. The instance token is the **transport gate only**
  and confers no scope: over the unix socket the CLI's `operator` claim stays
  cooperative (local trust, unchanged); over TCP the CLI authenticates as
  `operator` with that scope's secret (`DAIORAMA_SCOPE_TOKEN`; scope fixed
  `operator`; `scope_auth_required` when absent). The starter scope set
  includes `operator` and `dashboard`; scope secrets are generated 0600 at
  `config setup` / the first `daiorama mcp --listen` startup, and `token show`
  prints the reminder that network callers need them. MCP callers default to
  own-sessions-only; broader visibility is per-scope config
  (`sessions.visibility`, scope files 08), never default.
- Scope definitions (quotas, manifests, audit flags) are files — see 08.
- **Observation is instance-scoped, not scope-scoped:** a caller admitted to
  the instance can observe any session whose id it knows (view streams, media
  artifacts). Scopes constrain control, listing, and history; observation is
  gated at the instance level — and by whether the relevant surface is
  exposed at all (05).
- Errors: `scope_unknown`, `scope_auth_required` (05).

## Foreign sockets and interop (B7)

The runtime-directory bind-mount trick is scoped to **CLI/RPC discovery**:
talking to a foreign, compatible **manager** is the stated feature. Guards:

- **Identity probe before adoption, always.** The probe is the tokenless
  `handshake` call (05) — strictly first; credentials are presented only
  after it. Instance ID lives at `<data>/<instance>/instance-id` (0600) and
  is returned by `handshake`; the first `--remote` attach to a new address
  records a TOFU pin in the client config; a later mismatch refuses with a
  distinct string unless explicitly re-pinned. The probe is a routing aid,
  not a trust decision — the trust decision is the credential the caller
  then presents and the operator's explicit `--remote`.
- **Attachment is explicit, never a side effect** of session creation.
- **A foreign compositor socket is not adoptable.** Hyprland's own socket
  found via a bind-mount does not become a session: no child, no PDEATHSIG, no
  ownership — whose session would it even be? Clear refusal, not best-effort.
- **Boundary-inversion hazard, documented as the guard's reason:** if a
  container CLI adopts the *host* manager, session RPCs execute on the host —
  desktops run outside the container and its isolation boundary. That hazard,
  not tidiness, is why adoption is explicit and probed.
- **The bind-mount grants instance trust.** Mounting the runtime directory
  into another container gives it the tokenless socket with cooperative scope
  claims (including `operator`) — a deliberate trust decision, not a side
  effect; shared instances point clients at TCP instead (10).

## Rate limiting (B9 — mechanism adopted at P0; numbers verified in the build, P4)

A session driven at inhuman control-call rates is itself a detection signal —
it cuts against the reason the project exists. Mechanism (adopted at the P0
reconcile; numbers verify in the build): a per-session token bucket over
**control-class** calls (input class; reads exempt), configured per scope:
`limits.rate { per_sec = 20, burst = 40, mode = pace | reject, max_wait_ms = 30000,
max_queued = 64 }`. Default `pace` delays excess calls to the bucket rate in arrival order
(input cadence stays human-plausible); `reject` fails `rate_limited` with `hint: retry after
<ms> ms`. Pacing is bounded on both axes: a wait beyond `max_wait_ms`, or a call arriving while
`max_queued` are already parked, is refused instead of queued — a flood degrades to refusals,
never to a parked army. The counter lives in the manager, keyed by session. Pacing is also the
natural place for future input-cadence shaping. Capture is exempt from the bucket but serialized
(one at a time per session).

## Posture statement (repeated here on purpose)

None of these config-level restrictions are a security boundary against a
caller with code execution in the same environment: the compositor's input and
capture protocols grant capability to any client holding a socket connection,
and a few lines of code bypass a manifest entirely. Quotas, manifests, and
tokens are harm reduction — bug-catching and clear steering for well-behaved
callers. The real security boundary is, and must remain, the container/OS
isolation layer around the whole thing.
