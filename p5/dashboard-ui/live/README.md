# Dashboard UI live leg

Every existing dashboard check speaks HTTP or CLI to the server: `dashboard/tests/e2e.rs` and
`tests/dashboard.rs` drive raw sockets, and the D1/D2/D5 live legs drive the CLI. None of them
executes `dashboard/ui/app.js`. The page is the one shipped surface where a disagreement between
the server's JSON and the page's expectations is invisible — a missing id, a renamed field or a
wrong query parameter shows up only when a browser runs the code. This leg runs it.

## What it drives

One container, one `dbus-run-session`, no mocks:

1. `config scope create dashboard --template dashboard`, then `daiorama serve` with
   `manager.bind: 127.0.0.1:8756` — the real manager, its frame protocol on TCP, the instance
   token and the scope secret minted at bind (07).
2. `daiorama new --label ui-leg` — a live Hyprland session, so the page has a real row to render.
3. The module installed the shipped way: a real `daiorama-dashboard-<version>-<triple>.tar.gz`
   through `daiorama dashboard install --artifact --sha256`, then `connect` / `token set` /
   `scope-token set` (the scope secret is a module-local verb; the core forwards
   serve/connect/connections/token only).
4. `daiorama dashboard serve` — the installed module's own server on 127.0.0.1:8760, serving the
   installed `ui/` tree.
5. Chromium (`--headless=new`, DevTools on 9222) loads `http://127.0.0.1:8760/`. `probe-ui.py`
   speaks CDP over the DevTools websocket with the standard library only — no third-party
   package has to exist in the image — and asserts the rendered DOM: the manager row and its
   readiness, the session table, a row click into the detail pane, the history list, and Open
   view.
6. The URL the click opens is then fetched from the shell exactly as the page built it, so the
   leg proves the click lands on the real noVNC page and not only on a note.

`probe-ui.py` fails the leg on an uncaught exception or a console error, so a page that renders
"almost right" while throwing in a callback does not pass.

## Run

Inside a container that has Hyprland, chromium and python3 (the run in `evidence/` used the L1
image), under `dbus-run-session`:

```
bash p5/dashboard-ui/live/run-dashboard-ui.sh <daiorama> <artifact>
```

The container mounts the two binaries and the module's release asset:

```
tar czf daiorama-dashboard-0.1.0-x86_64-unknown-linux-gnu.tar.gz -C stage daiorama-dashboard ui
```

The view asset tree is staged from the image's `/usr/share/novnc` to `<data>/novnc` (production
does the same with `packaging/novnc/fetch.sh`); without it the front answers its pinned
`view assets not installed` refusal and Open view cannot be followed to a page.

## Result at `3ce12c7`

- `evidence/dashboard-ui-before-3ce12c7.log` — the leg's first run, on the page as merged:
  1 failure, `the opened target carries the credential exactly once :: credential= x2`.
- `evidence/dashboard-ui-3ce12c7.log` — the same leg after the fix: 34 assertions, 0 failures,
  `view_front_http=200 bytes=17810`.

The defect: `get_view` answers an address that already carries the credential
(`{base}/?credential=…`) *and* the credential as its own field (05, 07), and `openView()` appended
the field to the address unconditionally, so Open view opened
`http://127.0.0.1:6080/?credential=X&credential=X` — the secret twice in the URL, and an address
whose authentication depends on which duplicate the front reads. `viewTarget()` now sets the
parameter on the parsed address, which leaves exactly one copy whether or not the address came
with one. The assertion that catches it lives in the probe, so a regression fails the leg instead
of sliding through.
