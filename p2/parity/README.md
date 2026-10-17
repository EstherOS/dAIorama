# P2 — parity audit: frozen docs vs the built surface

Ladder step 2. The spec set is frozen; this pass checks the build against it surface by
surface, records the delta, and fixes drift found on the way. Live probes ran against the
shipped manager off-mode over a real socket (containment off — cgroupfs read-only here; the
`limits_unavailable` refusal itself is the pinned 04 string), plus the built binary's own help
tree and first-run config. Raw outputs in `evidence/`.

## Verdict

| surface | source of truth | result |
|---------|-----------------|--------|
| CLI command tree + flags | 04 §Commands, §Global flags | **match**: 33 leaf commands and their parents, 19 distinct flags — every documented command/flag present, nothing undocumented beyond clap's own `help` |
| pinned defaults | 04 §Pinned defaults | **match** where built: instance `default`, socket path, resolution/scale, `view.port_base` 6080, reaper idle `off` / `sweep_dead` `on`, history 8 × 4 MiB / 7 d, media TTL 600, token path, `manager.start_mode auto`, `renderer.allow_software off`, drain `--timeout` 300 |
| error codes | 05 §Errors (25 codes) | **match**: every documented code exists as a literal in the build |
| read shapes | 05 §Pinned read shapes | **match**: instance status keys exactly `{state, mode, pid, socket, versions{binary, protocol, min_supported}, sessions{running, dead}, drain{active, remaining}, instance_id}`; list page `{sessions}` on an empty instance (04: `next_cursor` only when a page truncates — truncation not exercised live: no sessions on this box) |
| refusal strings | 04 §Refusal and error strings | **match**: 16 bullets, byte-for-byte (doc-reading unit test already pins them) |
| parameter handling | 05 §Errors (unknown keys = `bad_request`) | **drift found and fixed** — see P-1 |

## P-1 (fixed) — no-parameter methods answered calls that carried parameters

`status`, `readiness`, `config_reload` and `drain` take no parameters, but their dispatch arms
ignored the params entirely: a caller that sent a stray key (a typo'd name, a parameter from a
different method, a future client's addition) got a normal answer instead of a refusal. That is
the silent-wrong-answer class the P2 hardening passes removed elsewhere — `list_sessions`,
`session_status`, `config_get` and `config_set` already refuse unknown keys with
`bad_request`; these four did not.

Live, before (`evidence/noparam-before.txt`):

```
status         {"bogus": 1}       -> ok
readiness      {"bogus": 1}       -> ok
config_reload  {"bogus": 1}       -> ok
drain          {"wait": true}     -> ok
```

After (`evidence/noparam-after.txt`):

```
status         {"bogus": 1}       -> bad_request  unknown parameter "bogus"; this method takes no parameters
readiness      {"bogus": 1}       -> bad_request  unknown parameter "bogus"; this method takes no parameters
config_reload  {"bogus": 1}       -> bad_request  unknown parameter "bogus"; this method takes no parameters
drain          {"wait": true}     -> bad_request  unknown parameter "wait"; this method takes no parameters
```

Fix: one helper (`no_params`) on the dispatch path for exactly those four methods; `null`,
absent and `{}` still mean "no parameters". The new string is pinned in `fixtures/README.md`'s
frame contract and the README-reading test covers it, so wording drift fails the build. Unit
test covers all four methods in both directions (refused with a key, accepted without).

## Observations (open, low severity)

- **O-1 — `config set` on a restart-only key names reload.** With a manager running, `config
  set manager.bind …` prints "a running manager keeps the old value until 'daiorama config
  reload' (or restart)", but reload reports `manager.bind` as restart-only and does not apply
  it. 04 asks the output to state *which of the two* applies; today it always names reload
  first. Recoverable (reload names the restriction), wording touch on an operator string —
  left for a deliberate decision rather than folded into this pass.
- **O-2 — `config.lock` is not in 08's filesystem table.** The config write lock (P2's race
  fix) leaves a sibling `<config>.lock` file beside `config.ron`. 08's "exact paths" table
  does not mention it. One line in 08 at the next docs touch.
- **O-3 — unbuilt methods answer `internal` before validating params.** `daemon_end` with a
  stray key answers `internal` (`not implemented yet`), not `bad_request`: the method's pinned
  parameter surface does not exist yet, so there is nothing to validate against. Revisit when
  each method lands.

## Unbuilt surface inventory (P3 queue)

Documented, CLI/registry surface present, manager answers `internal` (`not implemented yet`):

| surface | doc | note |
|---------|-----|------|
| `daiorama mcp` (stdio and `--listen`) | 05 §The bridge | entry point exists, exits `not implemented yet`; bridge is P3 by its own module header |
| `list_capabilities` | 05 | always-on read; absent build |
| `get_view`, `stop_view` (+ view front, per-view credential) | 05 §Views, 06, 07 | view front (wayvnc/noVNC) not reachable yet |
| `screenshot`, `clip_video`, `clip_audio` | 05 §Capture | media endpoint `/media/*`, TTL + single-use semantics absent |
| input tools — `pointer_move`, `pointer_click`, `pointer_button_down/up`, `pointer_scroll`, `key_tap`, `key_down`, `key_up`, `type_text` | 05 §First landing category | named and schematized in the registry; dispatch stub |
| `list_history` | 05, 06 | history storage exists runtime-side; read surface absent |
| `daemon_end` | 04, 10 | CLI `daemon end` surface present, manager verb stub |
| dashboard module | 04, 09 | CLI surface present; `module not installed` string pinned |

Built and live: `create_session` (spawner + registry), `list_sessions`, `session_status`,
`close_session`, `status`/`readiness`, `config_get`/`config_set`/`config_reload`, `drain`.

## Evidence

- `evidence/cli-tree.txt` — help tree of the built binary (command → flags).
- `evidence/defaults-config.ron` — first-run `config setup` output.
- `evidence/read-shapes.txt` — live `status`/`list`/`version` JSON.
- `evidence/noparam-before.txt`, `evidence/noparam-after.txt` — the P-1 battery against the
  pre-fix and fixed binaries (same script, fresh instance each).
