# P2 — the renderer gate: creation-time detection, refusal, marks (06 D6)

Raw evidence that the renderer policy holds end to end: at session creation the spawner
reads the active renderer out of the child compositor's instance log — a detected software
renderer refuses with the byte-pinned 04 string unless `renderer.allow_software = on` — and
every session record carries the detected mark (`verified` / `software` / `unverified`);
nothing is ever silently `verified`.

The mark rides back on the spawner trait's outcome (`SpawnOutcome` in `manager`); the
service writes it into the session record. `renderer.allow_software` is read from the live
config snapshot at each create (08 reload semantics), so a `config reload` flips the next
create's answer.

## Files

- `evidence/renderer-real.txt` — the `#[ignore]d` real runs in a fresh privileged container
  on the reference box (rootless podman, cgroup v2, render node), one invocation per leg:
  1. `real_containment_chain` **first** — the containment chain still green after the gate
     landed (the gate now runs inside that spawn path);
  2. `real_renderer_gate` — one real session (mutter → Hyprland): the raw instance-log line
     `GATE-RAW-LINE: DEBUG ]: Renderer: NV117` and the outcome mark `verified`;
  3. `real_chain_lifecycle_verbs` — create ×2 / list / status / close over the real socket.
  Post-run state: no compositor runtime dirs left behind, nsroot controllers enabled, no
  session scopes under the instance subtree.
- `evidence/renderer-real-spec.json` — create-spec for those runs (mount sources
  genericized — substitute your checkout/artifact paths). The test binary is built by the
  recipe below.

## Build / run recipe

Same as `p2/scope/`: build from the branch tip in a `rust:slim` container
(`rustup component add rustfmt clippy`; `cargo fmt --check`,
`cargo clippy --all-targets --locked -- -D warnings`, `cargo test --locked`,
`cargo test --no-run --locked`; the unit-test executable is copied out as
`daiorama-tests`), then execute in the L1 container:

```text
dbus-run-session -- ./daiorama-tests real_containment_chain --ignored --nocapture --test-threads=1
dbus-run-session -- ./daiorama-tests real_renderer_gate --ignored --nocapture --test-threads=1
dbus-run-session -- ./daiorama-tests real_chain_lifecycle_verbs --ignored --nocapture --test-threads=1
```

Container-ordering pitfall (hit while collecting this evidence): the containment probe's
keeper prep is one-shot per container — it moves the container's processes under
`/keeper`, after which any later probe sees a non-nsroot cgroup and refuses with
`controllers not delegated to this scope`. Run `real_containment_chain` before any other
cgroup-probing test in a fresh container (or alone).

## Result

All three runs green (`exit=0` each). The fresh `rust:slim` gate was green on the same tip:
`cargo fmt --check` clean, `clippy -D warnings` clean, full suite
`126 + 3 + 2 + 27 passed, 0 failed, 5 ignored` (the ignored five are the real-container
tests, listed above plus `real_box_probe` and the self-killing `pdeathsig_witness`).

## Stub coverage (in-tree)

`src/runtime/spawner.rs` — detection parsing against the exact evidence line shapes
(`... Renderer: NV117`, `... Renderer: llvmpipe (LLVM 22.1.8, 256 bits)`, absent/empty),
the software-family set, and the spawn-level legs with fake instance logs: GPU → `verified`;
llvmpipe with the override off → refusal carrying the 04 string byte-exact
(`strings::renderer_refused("llvmpipe")`) with the chain killed and nothing left behind;
llvmpipe with the override on → `software`; no log line → `unverified`.
`src/runtime/service.rs` — the record carries the spawner's mark; the live
`renderer.allow_software` value (including post-`config reload`) reaches the spawner.

## Notes

- The software **refusal** cannot be exercised end to end on this stack: there is no
  spawner knob to force llvmpipe for the child alone, and a whole-chain software env
  crashes the parent (pre-W2: aquamarine dmabuf v4-on-v3). The refusal is pinned by the
  stub tests above, and its detection input is the real software log shape from
  `p2/pre-w2/renderer/evidence/` (`Renderer: llvmpipe (LLVM 22.1.8, 256 bits)`).
- Deeper software-lane proof belongs to the W5 llvmpipe lane (its own env).
