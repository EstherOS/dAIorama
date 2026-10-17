# P3 W1 — the stdio MCP bridge, live against a real manager

`daiorama mcp` speaks MCP JSON-RPC on stdio and relays to the same manager the CLI drives.
This pass builds the local surface: framing, both handshake paths, the caller scope's exposed
manifest, `tools/call` gating plus relay, `list_capabilities`, the `-32022` version refusal, and
the scope/origin carriage the manager reads per call (05 §Scope and origin carriage). The
network mode (`--listen`) stays a typed surface until its slice lands.

## Raw drill — `evidence/bridge-live.txt`

Fresh root under `/tmp/dai-p3-live`, `limits.mode = off`, an `agent` scope seeded with own
visibility, `input` + `capture` grants and `max: 2`, then `daiorama mcp` driven over stdin.
Every response line is raw stdout; no rewriting.

What the run shows:

- `server/discover` answers the pinned server info and the `tools` capability only.
- `tools/list` returns exactly the scope's exposed manifest: the always-on five plus the
  granted `input`/`capture` tools; `get_view`/`stop_view`/`list_history` are absent because the
  grants do not include them.
- `list_capabilities` answers from the scope file plus the manager's live count —
  `quotas.sessions = {live: 0, max: 2}`, `audit` flags from the scope.
- `list_sessions` relays to the manager and comes back scoped (`{"sessions":[]}`).
- An unbuilt-but-pinned tool (`screenshot`) answers the tool-execution form: `isError: true`,
  `internal` / `not implemented yet`.
- A name outside the registry answers `bad_request` / `unknown tool "nope"` — dispatch-level,
  the rest of the session continues.
- A client pinned to `2025-06-18` is refused `-32022` with both version pairs in the message.
- Without `DAIORAMA_SCOPE`, `daiorama mcp` refuses before serving:
  `scope required: set DAIORAMA_SCOPE. (scope_required)`.

## Re-run

```
cargo build
python3 p3/bridge/evidence/repro.py > p3/bridge/evidence/bridge-live.txt
```

The script needs `target/debug/daiorama` (or `DAIORAMA_BIN`) and nothing else; it seeds and
tears down its own root, and it terminates the manager it spawns.

## Adversarial battery — `evidence/probe-stdio.py`

Raw stdin classes, one process per probe: null/missing id, JSON-RPC batch, missing/typed
method, `tools/call` with no/array params, a notification carrying a call, a legacy
`initialize`, a non-UTF-8 line, an over-cap line (8 MiB + 1), a relay with no manager, and a
bad-then-good pair to prove the connection survives a dispatch-level refusal. Found the null-id
silence (`"id": null` was treated as a notification and never answered); the fix and its pin
are in `src/mcp/` and `fixtures/README.md`. `evidence/probe-stdio-prefix.txt` is the battery on
the pre-fix code (null id: no line at all) and `evidence/probe-stdio-fixed.txt` the same battery
after it (null id answers `-32600`; an absent id stays silent).
