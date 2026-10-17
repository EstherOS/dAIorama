# P2 — config-write race: one writer at a time, atomic replace

Concurrency stress battery (fourth pass — the first with parallel clients) and its finding.
The pass ran the shipped manager off-mode over a real socket, driven only through raw frames
plus CLI invocations, against a fresh instance per run.

## Finding — concurrent `config set` writers clobber each other (lost updates, torn reads, file corruption)

`config set` was load → modify → `std::fs::write` with no lock, on a multi-threaded runtime
(one task per connection) and with the same shape in the local CLI path. Two overlapping
writers therefore:

- **lost updates** — each wrote its whole snapshot, so the later save silently dropped the
  other writer's key; the monitors caught hundreds of in-act regressions in one round
  (`pre-fix-battery-v2.txt`, round 0: 837 regressions, final LOST), and the cross-process
  drill (CLI process loop vs RPC loop) lost a write too (`pre-fix-cross-process.txt`,
  `history.segments` want 429 got 428);
- **torn reads** — readers of the file mid-truncate got hard parse errors
  (`config load error: 1:1: Expected opening '(' for struct CoreConfig`);
- **persistent corruption** — interleaved in-place writes can leave a spliced file that fails
  to parse forever: the run's round 0 bricked the file and every later operation (rounds 1–3,
  wide-file drills, cross-process writers, readers) failed until hand-repair — an unreadable
  core config means no session can start (`pre-fix-corrupt-config.ron` +
  `pre-fix-corrupt-read.txt`; `pre-fix-battery-v2.txt`).

## Fix (this branch) — `update_config`: lock → load → apply → save; `save`: temp + rename

- `config::lock_config_write` takes an exclusive `flock` on a sibling `<file>.lock` across the
  whole read-modify-write (kernel-released on crash — the manager-lock pattern, 10).
- `config::update_config` is the one RMW entry; both `config set` paths use it (local CLI and
  the manager's RPC arm). `config edit` / `config setup` first-run creates take the lock too.
- `CoreConfig::save` replaces the file atomically (temp sibling + `rename`), so readers see
  the old or the new file, never a partial one, and a crash mid-write cannot truncate the
  original. Readers (`config get`, reload, other processes) need no lock.
- Regression tests: `config::tests::concurrent_updates_keep_every_key`,
  `config::tests::save_replaces_atomically_and_leaves_no_temp_litter`,
  `runtime::service::tests::concurrent_config_sets_keep_both_keys`.

## Result — same battery, fixed binary: all green

`post-fix-battery.txt` (same script revision as `pre-fix-battery-v2.txt`):

- (a) 24 workers × 40 mixed requests: no cross-talk, every response matched; fresh connections
  answered in <1 ms under load.
- (b) fd churn clean (baseline 11 → 11 after 400 connects).
- (c) RPC/RPC race, 4 rounds × 250 writers each, monitors on both keys: **0 violations,
  0 lost finals** (before: round 0 = 837 + corruption).
- (c2) wide-file variant (~256 KiB config, ms-scale RMW windows): 0 violations, 0 lost finals
  (before: LOST + parse errors).
- (d) cross-process CLI/RPC race: both writers' final values present (before: loss).
- (e) 410 `config get` reads during a write loop: 410 ok, 0 errors (before corruption run:
  0 ok, 403 errors).

## Repro recipe

```
# fresh instance root
ROOT=<scratch>/dr-liveX; mkdir -p $ROOT/run $ROOT/cfg $ROOT/data
export XDG_RUNTIME_DIR=$ROOT/run XDG_CONFIG_HOME=$ROOT/cfg XDG_DATA_HOME=$ROOT/data
DAIORAMA_BIN=<built daiorama>   # same binary drives the manager and the CLI legs
$DAIORAMA_BIN --instance probe4 config set limits.mode off
$DAIORAMA_BIN --instance probe4 serve &                # wait for manager.sock
DAIORAMA_BIN=$DAIORAMA_BIN python3 probe-concurrency.py \
    $ROOT/run/daiorama/probe4/manager.sock <manager-pid>
```

Under concurrency, `--instance` and the XDG roots must be the probe's own (tests do this via
injected env); the probe never touches the default instance.

## Files

- `probe-concurrency.py` — the battery (sections a–e; optional third arg filters by label).
- `evidence/pre-fix-battery.txt` — first repro run (earlier detector revision: round 0 and the
  LOST finals are the clean signal; the monitor counts in later rounds also include
  round-boundary resets).
- `evidence/pre-fix-battery-v2.txt` — same detector revision as the post-fix run: round 0
  bricks the file (837 regressions, torn-read errors); rounds 1–3 and (c2)/(d)/(e) then operate
  against the dead config (every op fails → LOST).
- `evidence/pre-fix-cross-process.txt` — fair CLI-vs-RPC run on a healthy file: one write lost.
- `evidence/pre-fix-corrupt-config.ron` — the persistently unparseable file a run left behind
  (extra `)` splice, stale mid-race values).
- `evidence/pre-fix-corrupt-read.txt` — that file's read failing (`Non-whitespace trailing
  characters`).
- `evidence/post-fix-battery.txt` — the fixed binary on the same battery: all green.

Gate on the fixed tree: `cargo fmt --check` clean · `cargo clippy --all-targets -- -D warnings`
clean · `cargo test` — 141 (+5 ignored) · 4 · 2 · 27 passed, 0 failed (three new tests).
