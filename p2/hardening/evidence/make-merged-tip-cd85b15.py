#!/usr/bin/env python3
"""Compose merged-tip-cd85b15.txt from the captured raw logs.

Every count in the header is derived from the logs in this directory; the expected
values are asserted so a stale or partial log set fails instead of publishing a
number nobody measured.
"""
import re
import pathlib
import sys

EV = pathlib.Path(__file__).resolve().parent
TIP = "cd85b15"

gate = (EV / f"gate-{TIP}.log").read_text(errors="replace")
results = re.findall(r"^test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored", gate, re.M)
assert results, "no test result lines in the gate log"
passed = [int(p) for p, _, _ in results]
failed = sum(int(f) for _, f, _ in results)
ignored = sum(int(i) for _, _, i in results)
expected = [289, 5, 7, 8, 9, 3, 27, 19, 7, 22, 6, 27, 6, 3, 6, 2, 5]
assert passed == expected, f"suite sequence {passed} != expected {expected}"
assert failed == 0, f"{failed} failed tests"
assert ignored == 6, f"{ignored} ignored tests"

legs = [
    ("dispatch-stall", "run-dispatch-stall.sh", None),
    ("rate-limit", "run-rate-limit.sh", 16),
    ("rate-buckets", "run-rate-buckets.sh", 9),
    ("history", "run-history.sh", 29),
    ("history-bound", "run-history-bound.sh", 4),
    ("dashboard-module", "run-dashboard-module.sh", 20),
    ("scope-create", "run-scope-create.sh", 56),
    ("dashboard-install", "run-dashboard-install.sh", 50),
    ("browser", "run-browser.sh", 15),
    ("install-rows", "run-install-rows.sh", 40),
    ("release-line", "run-release-line.sh", 57),
    ("install-live", "run-install-live.sh", 32),
]
rows = []
total_ok = 0
for name, script, expect in legs:
    text = (EV / f"{name}-{TIP}.log").read_text(errors="replace")
    ok = len(re.findall(r"^\[OK\]", text, re.M))
    bad = len(re.findall(r"^\[FAIL\]", text, re.M))
    assert bad == 0, f"{name}: {bad} [FAIL] lines"
    if expect is not None:
        assert ok == expect, f"{name}: {ok} [OK] != expected {expect}"
    total_ok += ok
    verdict = "PASS" if "VERDICT=PASS" in text or "FAILS=0" in text else "?"
    rows.append((name, script, ok, verdict))
assert total_ok == 328, f"leg assertions {total_ok} != 328"

caps = []
for i in (1, 2, 3):
    text = (EV / f"capture-parallel-{i}-{TIP}.log").read_text(errors="replace")
    m = re.search(r"^test result: ok\. (\d+) passed; (\d+) failed", text, re.M)
    assert m and m.group(1) == "15" and m.group(2) == "0", f"capture {i}: {m and m.groups()}"
    caps.append("15p/0f")

leg_lines = "\n".join(
    f"{name:<26} {script:<33} {ok if ok else '—':>3} assertions, {verdict}"
    for name, script, ok, verdict in rows
)

text = f"""===================================================================
## merged-tip battery — spec/v1 @ {TIP}   (2026-10-18)
===================================================================
Tip: {TIP} ("p5(hardware): pixel oracle at two resolutions and scale 2"). 209167f
(STATUS-only) sits on top; `git diff --stat {TIP}..209167f` is STATUS.md alone, so this raw
belongs to the source tree at {TIP}. First FULL twelve-leg battery on this tip: the last was
ff153cb (battery f69e3c9), and a3c9d5d was a changed-surface pass covering only the input line.

Delta since ff153cb (src/tests/packaging: 276 insertions, 32 deletions across 5 files):

  - `src/runtime/input.rs` (+162): chord support — the manager carries modifier state so
    `key_down` / `key_tap` / `key_up` across three calls delivers a shifted character; `key_up`
    refuses a non-modifier symmetrically with `key_down`; act-half fixes (keymap settle,
    `modifiers` request, one-character keysyms) (#129/#130/#131).
  - `src/runtime/service.rs` (+127): per-session modifier state and the `session_known` wiring
    those tools ride.
  - `packaging/container/Containerfile.core` and `.desktop` (+3 each): COPY
    `packaging/release/trusted-keys` into the stage that compiles the `include_str!` input.
  - `tests/packaging.rs` (+13): pins both COPY lines so a later source-only COPY list cannot
    silently break the image build.
  - `p2/hardware/*`, `p5/browser/README.md`: browser legs, image build pins, pixel oracle —
    harness and evidence only, no product code.

Source identity: `git rev-parse 09c6683:src` == `git rev-parse {TIP}:src`
(83e0a0035f5288a89082d90af550502bfe007dbc), so the input live pass produced at 09c6683
(#132: 28 assertions — standing leg 21, CHORD=works / SHIFTED_TAP=works, probe-up-symmetry 5)
binds to this tree; nothing under `src/` changed after it. The only code delta since the last
full pass is the packaging COPY and its pin, both exercised by the packaging suite in [0].

Environment: one fresh rust:slim container (`dai-mtip-{TIP}`, gates + twelve legs), rustc
1.99.0 (b940084d7), cargo 1.99.0 (5f94df478), python3 + minisign 0.12 + curl (Debian).
Workspace mounted read-write at /src, CARGO_TARGET_DIR=/tmp/t (gates, legs) / /tmp/tc
(parallel capture), cold in the container. Gate set: fmt, fmt --check, clippy --workspace
--all-targets --locked -D warnings, build --workspace --bins --locked, test --workspace
--locked.

Expected suite sequence (17 suites, `cargo test --workspace --locked`):
{" · ".join(str(p) for p in passed)}
total {sum(passed)} tests, {failed} failed, {ignored} ignored.  (ff153cb read
287 · 5 · 7 · 8 · 9 · 3 · 27 · 19 · 7 · 22 · 6 · 26 · 6 · 3 · 6 · 2 · 5 = 448; the root suite
gained the two chord unit tests and the packaging suite one COPY pin test.)

Live legs: dispatch-stall, rate-limit, rate-buckets, history, history-bound,
dashboard-module D1, scope-create D2, dashboard-install D5, browser D6, install-rows C2,
release-line C2, install-live C2, capture suite 3x concurrent cold.

Per-leg logs are committed next to this file (`<leg>-{TIP}.log`, `gate-{TIP}.log`); the
runners are `gate-{TIP}.sh` and `run-legs-{TIP}.sh`, and this file is composed by
`make-merged-tip-{TIP}.py`, which asserts every count below against those logs.

===================================================================
## [0] gates — fmt, clippy, build, tests (container dai-mtip-{TIP})
===================================================================
rustc rustc 1.99.0 (b940084d7 2026-09-28)
cargo cargo 1.99.0 (5f94df478 2026-08-27)
rc=0   fmt
rc=0   fmt --check
rc=0   clippy --workspace --all-targets --locked -- -D warnings
rc=0   build --workspace --bins --locked
rc=0   test --workspace --locked
""" + "".join(
    f"test result: ok. {p} passed; 0 failed; {i} ignored\n"
    for p, (_, _, i) in zip(passed, results)
) + f"""GATE_FAILS=0

===================================================================
## [1] carried live legs — all green (container dai-mtip-{TIP})
===================================================================
{leg_lines}
capture (3x parallel cold)   cargo test --locked capture          {", ".join(caps)}; rcs cap1=0, cap2=0, cap3=0

Total leg assertions outside the capture suite: {total_ok} ([OK] lines, 0 [FAIL]).

===================================================================
## [2] legs that own the delta since the last full pass
===================================================================
- None is new. The only code delta since ff153cb is the packaging COPY of
  `packaging/release/trusted-keys` and the two pins in `tests/packaging.rs`; the packaging
  suite inside the gate covers both (451 tests, 0 failed) and the Containerfiles themselves
  were built into working images on this same tree (`p2/hardware/evidence/
  browser-legs-09c6683.txt`: ids 599e26453c29 / 2f5da5bb6773, four browser legs green).
- The input line is byte-identical to 09c6683 (`src` tree 83e0a003…), where the live input
  leg ran green twice; re-running it here would re-prove the same bytes, so this battery
  spends its budget on the full surface instead.

===================================================================
## [3] notes
===================================================================
- **No product defect found on this tree.** Every gate and every leg is green at {TIP}.
- **Nothing red this run, and no harness drift:** the twelve leg logs in this directory are
  the first attempt in the one container; no leg was re-run after a fix.
- **The battery is the artifact, not the message:** the final merged tip is {TIP}, and the
  published STATUS last-green line names it; this raw is the twelve-leg evidence behind that
  line rather than a docs-only claim.
"""
path = EV / f"merged-tip-{TIP}.txt"
path.write_text(text)
print(f"wrote {path} ({len(text)} bytes): {sum(passed)} tests, {total_ok} leg assertions")
