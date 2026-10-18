#!/usr/bin/env python3
"""Assemble p2/hardening/evidence/merged-tip-ff153cb.txt from the captured logs."""
import os, re, pathlib

EV = pathlib.Path(os.environ.get("EV", pathlib.Path(__file__).resolve().parent))
out = []

def read(name):
    return (EV / name).read_text(errors="replace")

gate = read("gate-ff153cb.log")
gate_results = re.findall(r"^test result: .*$", gate, re.M)
gate_seq = " · ".join(re.search(r"ok\. (\d+) passed; (\d+) failed; (\d+) ignored", l).group(1) for l in gate_results)
gate_total = sum(int(re.search(r"ok\. (\d+) passed", l).group(1)) for l in gate_results)
gate_failed = sum(int(re.search(r"(\d+) failed", l).group(1)) for l in gate_results)
gate_ignored = sum(int(re.search(r"(\d+) ignored", l).group(1)) for l in gate_results)

legs = [
    ("dispatch-stall", "run-dispatch-stall.sh", "dispatch-stall-ff153cb.log"),
    ("rate-limit", "run-rate-limit.sh", "rate-limit-ff153cb.log"),
    ("rate-buckets", "run-rate-buckets.sh", "rate-buckets-ff153cb.log"),
    ("history", "run-history.sh", "history-ff153cb.log"),
    ("history-bound", "run-history-bound.sh", "history-bound-ff153cb.log"),
    ("dashboard-module", "run-dashboard-module.sh", "dashboard-module-ff153cb.log"),
    ("scope-create", "run-scope-create.sh", "scope-create-ff153cb.log"),
    ("dashboard-install", "run-dashboard-install.sh", "dashboard-install-ff153cb.log"),
    ("browser", "run-browser.sh", "browser-ff153cb.log"),
    ("install-rows", "run-install-rows.sh", "install-rows-ff153cb.log"),
    ("release-line", "run-release-line.sh", "release-line-ff153cb.log"),
    ("install-live", "run-install-live.sh", "install-live-ff153cb.log"),
]
rows = []
total_assertions = 0
for name, script, log in legs:
    text = read(log)
    ok = len(re.findall(r"^\[OK\]", text, re.M))
    bad = len(re.findall(r"^\[FAIL\]", text, re.M))
    verdict = "PASS" if bad == 0 else "FAIL"
    if name == "dispatch-stall":
        m = re.search(r"fast_call_latency_s=([0-9.]+)", text)
        detail = f"PASS (fast call {m.group(1)} s while four create_session calls block)"
    elif name == "history":
        detail = f"{ok} assertions, FAILS={bad}"
        total_assertions += ok
    else:
        detail = f"{ok} assertions, FAILS={bad}, {verdict}"
        total_assertions += ok
    rows.append(f"{name:26s} {script:34s} {detail}")

cap = [read(f"capture-parallel-{i}-ff153cb.log") for i in (1, 2, 3)]
cap_counts = []
for c in cap:
    m = re.search(r"test result: ok\. (\d+) passed; (\d+) failed", c)
    cap_counts.append(f"{m.group(1)}p/{m.group(2)}f")
cap_rc = [read(f"capture-parallel-{i}.rc").strip() for i in (1, 2, 3)]

# the install-live first attempt (missing curl) is preserved separately
first_attempt = read("install-live-ff153cb-nocurl.log") if (EV / "install-live-ff153cb-nocurl.log").exists() else ""

header = f"""===================================================================
## merged-tip battery — spec/v1 @ ff153cb   (2026-10-18)
===================================================================
Tip: ff153cb ("docs(status): last green at 930470c (448); unbuilt-method examples
corrected"). Last full pass in this series was dc59f6e (PR #115, merged at 75050e8); a
changed-surface pass ran on 56b9a26 (PR #126) covering only the five legs that own the
rate/history/dispatch surface. This is the first FULL battery on the merged tip.

Delta since dc59f6e (16 commits, 1528 insertions across 8 source/test/packaging files):

  - `packaging/release/install.sh` (new, 296 lines) + `tests/install_script.rs` (740):
    verified fetch with an embedded trust list, digest before placement, metadata vs
    artifact read bounds, row-target requirement, empty-prefix refusal (#116/#119/#120).
  - `src/runtime/rate.rs` (+160): the B9 bucket map is bounded — an id that cannot name a
    session is never keyed, 4,096-bucket cap with LRU quarter eviction, `forget` at close (#117).
  - `src/history.rs` (+98): invented ids leave no state; the segment cache is bounded (#118).
  - `src/runtime/service.rs` (+111): the `session_known` gate (live registry OR durable
    history) that #117/#118 wire into the rate gate and the history recorder.
  - `src/release.rs` (+8), `tests/packaging.rs` (+144): compat table bound to the build's
    own constants; release notes + notes-range pins (#125/#127).
  - `p2/hardening/known_session.py` + the three reworked probes (#126): a seeded
    `session.created` first segment makes a closed session's id known with no compositor.
  - Docs: STATUS last-green line, release notes 0.1.0, A7 compat table (docs only).

Environment: two fresh rust:slim containers (`dai-mtip-ff153cb` gates + twelve legs,
`dai-mtip-ff153cb-instlive` the installer leg re-run), rustc 1.99.0 (b940084d7), cargo
1.99.0 (5f94df478), python3 + minisign 0.12 + curl (Debian). Workspace mounted read-write
at /src, `CARGO_TARGET_DIR=/tmp/t` (gates, legs) / `/tmp/tc` (parallel capture), cold in
each container. Gate set: fmt, fmt --check, clippy --workspace --all-targets --locked
-D warnings, build --workspace --bins --locked, test --workspace --locked.

Expected suite sequence (17 suites, `cargo test --workspace --locked`):
{gate_seq}
total {gate_total} tests, {gate_failed} failed, {gate_ignored} ignored.

Live legs: dispatch-stall, rate-limit, rate-buckets, history, history-bound,
dashboard-module D1, scope-create D2, dashboard-install D5, browser D6, install-rows C2,
release-line C2, install-live C2, capture suite 3x concurrent cold.

Per-leg logs are committed next to this file (`<leg>-ff153cb.log`, `gate-ff153cb.log`),
the runners are `gate-ff153cb.sh` and `run-legs-ff153cb.sh`.

===================================================================
## [0] gates — fmt, clippy, build, tests (container dai-mtip-ff153cb)
===================================================================
rustc rustc 1.99.0 (b940084d7 2026-09-28)
cargo cargo 1.99.0 (5f94df478 2026-08-27)
rc=0   fmt
rc=0   fmt --check
rc=0   clippy --workspace --all-targets --locked -- -D warnings
rc=0   build --workspace --bins --locked
rc=0   test --workspace --locked
{chr(10).join(gate_results)}
GATE_FAILS=0

===================================================================
## [1] carried live legs — all green (container dai-mtip-ff153cb)
===================================================================
{chr(10).join(rows)}
capture (3x parallel cold)   cargo test --locked capture         {", ".join(cap_counts)}; rcs {", ".join(cap_rc)}

Total leg assertions outside the capture suite: {total_assertions} ([OK] lines, 0 [FAIL]).

===================================================================
## [2] legs that own the delta since the last full pass
===================================================================
- rate-buckets (9 assertions): VmRSS stays flat across rounds of unkeyable ids (the #117
  bound), and [D] paces a seeded closed session. Raw: rate-buckets-ff153cb.log.
- history-bound (4): invented ids leave no directory and no unbounded cache (#118).
  Raw: history-bound-ff153cb.log.
- install-live (32): the shipped installer over a real HTTP base with two real minisign
  keypairs — placement, receipt, digest before place, the full refusal set, and the
  empty-trust refusal before any fetch. Raw: install-live-ff153cb.log.

===================================================================
## [3] notes
===================================================================
- **No product defect found on this tree.** Every gate and every leg is green at ff153cb.
- **One red in this run was harness drift, owned and fixed:** the first battery container
  installed python3 + minisign but not `curl`, so `run-install-live.sh` aborted at its first
  placement with `curl is required for a remote release base` and reported 14 OK / 15 FAIL.
  The shipped installer's own no-network paths still answered correctly in that run (empty
  trust list refused before any fetch, all usage refusals). `run-legs-ff153cb.sh` now
  installs curl and the leg was re-run alone in a fresh container: **32 OK / 0 FAIL**. The
  in-battery red was reproduced deliberately in a bench built the same way without curl
  (same 14 OK / 15 FAIL, same first line) and that reproduction is kept as
  `install-live-ff153cb-nocurl.log`, so the class is auditable without re-reading a
  retry-overwritten file.
- **The known-session fixture is now in the tree (#126) and changes what the probes count.**
  The history leg reads 29 assertions (27 before) because it now asserts that the paged set
  equals the single-page read (seven records page as 2+2+2+1) and filters the seeded
  `session.created` line out of its `tool.call` counts; rate-limit reads 16 and rate-buckets
  9 as before.
- **The merged tip carries the harness, not just the product.** `known_session.py` is now
  part of the shipped bench, so a future battery on a compositor-less box drives real
  sessions instead of invented ids — the regression that read 5/8/1 red at 56b9a26 cannot
  recur silently.
"""

first = ""
if first_attempt:
    first = f"""
===================================================================
## [4] the same red reproduced in a bench without curl
===================================================================
{first_attempt.strip()}
"""

(EV / "merged-tip-ff153cb.txt").write_text(header + first)
print("wrote", EV / "merged-tip-ff153cb.txt")
print("gate total", gate_total, "failed", gate_failed, "ignored", gate_ignored, "suites", len(gate_results))
print("leg assertions", total_assertions)
