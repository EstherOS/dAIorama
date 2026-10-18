# Views live leg

The view surface has unit coverage and one live appearance (the dashboard UI leg clicks Open
view once). Nothing exercises what 06 actually promises about the *lifecycle* under a real
operator: one view per session no matter how many clients ask at once, a generation that moves
on every start, ports released on stop, and — the promise this leg was written to test — a view
that dies is replaced by the next `get_view` with a new generation, while the session stays up.

## What it drives

One container, one `dbus-run-session`, no mocks:

1. `daiorama serve` (loopback socket only, `limits.mode = off`) and `daiorama new` — a real
   Hyprland session with the noVNC tree staged at `<data>/novnc`, as production does.
2. **Four concurrent `daiorama view --json` calls.** All four must answer one address and one
   generation; `ps` must show exactly one `wayvnc`, `ss` exactly one listener on the view port.
3. The front's gate live: HTTP 200 with the credential, 403 without it.
4. `view --stop`: the front port closes, wayvnc is gone, session detail reports no view.
5. `view` again: the generation moves, a new wayvnc runs, the new front answers 200.
6. **`kill -9 wayvnc`** — the crash a real desktop sees. The session must stay `running`
   (view down ≠ session dead), session detail must not claim a live view, and the next
   `get_view` must answer a **new generation** whose front serves the page.
7. `close`: no wayvnc and no view port left behind.

## Result at `9619e23`

The leg was written against the merged tip and its first run found a defect.

- `evidence/views-live-before-9619e23.log` — 21 assertions, **1 failure** (20 OK):
  `kill -9 wayvnc` leaves the manager handing the dead view back. `session_status` still
  reports `view: running`, the front keeps answering HTTP 200 on the stale address (the page
  loads; the RFB relay is a dead socket), and `get_view` answers the **same generation** — so
  the address never becomes detectably stale and the operator's reload can never recover
  without a manual `view --stop`.
- `evidence/views-live-after-9619e23.log` — 23 assertions, **0 failures**. After the fix:
  session detail reads `view: down` with the old generation, the stale front is closed
  (HTTP 000), and the next `get_view` answers generation 3 with a working front (HTTP 200).

The defect: `ViewManager::running` was a pure map read, so a `wayvnc` that exited stayed a
view. 06 pins the opposite ("view death marks the view `down` … the next `get_view` starts a
fresh view (new generation)"). `running` now reaps an exited child (front stopped, entry
dropped), and `session_status` / `list_sessions` absorb the death on read — the same shape as
the browser-renderer line — so neither read surface reports a dead view as live.

## What the rest of the leg pins (all green before and after)

- Four concurrent `get_view` calls answer one address and one generation; one `wayvnc`, one
  listening front. The per-session start lock holds under a real browser-grade race.
- The front's credential gate live: 200 with, 403 without.
- `--stop` releases the port inside 3 s, kills wayvnc, and clears the session's view.
- A restart moves the generation and serves on the new front.
- `close` leaves no wayvnc and no view port behind.

## Run

Inside a container that has Hyprland, python3, curl and the noVNC tree (the L1 image), under
`dbus-run-session`:

```
bash p5/views/live/run-views-live.sh <daiorama>
```

The container mounts a worktree at `/src` and a scratch directory at `/out`; the leg writes
`/out/logs-views-<label>/` and its own log lives beside it. The image has no `ss`/`netstat`, so
port liveness is checked by connect (`curl` exit 7 = refused), not by socket-table parsing.

