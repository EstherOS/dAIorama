# Browser launcher — pre-read (P5)

Status: **unbuilt.** 06 pins the browser-side half of the renderer policy (D6) to a session
launcher at `/usr/local/bin/daiorama-browser`, and 09's desktop artifact is gated on that file
existing. This note settles the channel, the contract and the file the manager reads, so the
implementation is a code step rather than another design argument.

## What 06 asks of the launcher

- Browser launches go through the launcher; it is the documented launch route in the desktop
  image (09), and the bare-host channel has no such binary — an unrouted browser leaves the
  session `renderer: unverified`, never silently verified.
- The launcher captures **the browser's own rendered-GL renderer string** and writes it to a
  manager-readable file in the session runtime dir; a software result marks the session
  `renderer: software` and raises the warning.
- No automation protocol is involved: no CDP, no WebDriver, no `navigator.webdriver`, no
  headless flags (01). The browser is the same real browser the agent's session shows.

## The capture channel (measured, not assumed)

06 says "from its logging channel". Measured on the reference box in the L1 image (Hyprland
0.56.2, chromium from sid, real render node): Chromium's stderr carries **no** unmasked
renderer at `--v=1` (2333 lines), `--v=2` (2480) or `--vmodule='*gl*=3,*gpu*=3'` (2301) — zero
matches for `GL_RENDERER`, `unmasked_renderer` or `ANGLE`. The P1 mechanism still works: a page
that reads `WEBGL_debug_renderer_info` and writes the string into `document.title` is reported
verbatim by the compositor:

    title: dai-webgl:RENDERER: ANGLE (Mesa, NV117, OpenGL ES 3.2) | VENDOR: … | VERSION: …

Raw: `p5/browser/evidence/channel-probe.txt`. So the launcher reads the **window title through
the compositor's IPC** (`hyprctl clients`), not stderr. That is a correction to 06's wording,
not to its intent: the title is the browser's own report of the string, produced without any
automation protocol, and it is the only channel that carries it.

## Launcher contract

- **Binary:** `daiorama-browser`, a second bin of the root crate (`src/bin/daiorama-browser.rs`),
  so `cargo build --release --locked -p daiorama` already produces it. Installed to
  `/usr/local/bin/daiorama-browser` by the desktop image only.
- **Invocation:** `daiorama-browser [OPTIONS] <URL> [-- <browser args>]`.
  - `--browser <path>` (default `chromium`, or `$DAIORAMA_BROWSER`)
  - `--profile <dir>` (default `<data root>/browser`; the persisted profile of 09)
  - `--renderer-file <path>` (default `$XDG_RUNTIME_DIR/browser-renderer`)
  - `--capture-timeout <secs>` (default 10)
- **Flow:** write a probe page to `$XDG_RUNTIME_DIR/browser-probe.html` (0600) that computes the
  renderer, sets `document.title = "daiorama-renderer: <string>"`, and navigates to `<URL>`;
  launch the browser on it as an app window (`--app=file://…`); poll the compositor's client list
  for a title with that prefix; write the renderer file; delete the probe page; keep supervising
  the browser.
- **Renderer file:** `<runtime dir>/browser-renderer`, one line `renderer=<string>`, mode 0600,
  written atomically (temp file + rename in the same directory). Absent when the capture failed.
- **Failure semantics:** no title within the timeout → no file, one warning line on stderr, the
  browser keeps running (the session stays `renderer: unverified`). Missing browser or compositor
  IPC → exit 127 with a named message and no file. A malformed/absent file is never read as
  "verified".
- **Signals and exit:** SIGINT/SIGTERM are forwarded to the browser; the launcher exits with the
  browser's status (128 + signal for a signalled child). A launcher that dies takes the browser's
  parent-death signal with it — no orphan.
- **Not in scope:** navigation control, page-load waits, input, capture, or window management
  beyond the probe window. The launcher starts a browser; the session's own tools do the rest.

## Manager read rule (recommended; contract pin is the peer's)

The manager already knows the session's runtime dir. On session detail/status it reads
`<runtime dir>/browser-renderer` when present:

- a software string → move the session's `renderer` mark to `software` (one-way; a later hardware
  string never upgrades a mark), and append a `renderer.software` history record whose detail is
  the browser string;
- a hardware string or a missing/malformed file → nothing.

05's session object carries `renderer` but no warning field. The mark flip needs no contract
change; a spoken warning in the status payload would. Recommendation: land the mark flip +
history record with the launcher, and treat the warning field as its own 05 pin if it is wanted —
the refusal string family (04) is about creation, not a live session.

## Image wiring (desktop artifact, 09)

`Containerfile.desktop`: `FROM` the core artifact, install the pinned Chromium package, copy
`target/release/daiorama-browser` to `/usr/local/bin/daiorama-browser`, `CMD ["daiorama","serve"]`.
The browser profile stays under `<data>/browser` — a persisted volume, never an image layer (09
data rule). The core image is unchanged: no Chromium, no launcher, no default command.

## Acceptance (implementation PR)

1. **L1, real session:** `daiorama-browser <url>` → the window appears, `<runtime>/browser-renderer`
   exists with mode 0600 and the real string (`ANGLE (Mesa, NV117, …)` on the reference box), the
   probe page is gone, and the browser is on `<url>`.
2. **No title:** a stub browser that never sets a title → no file, one warning, and the session
   keeps `renderer: unverified`.
3. **Signals:** SIGTERM to the launcher ends the browser and leaves no orphan; the launcher exits with the browser's status (a clean Chromium shutdown is 0; 128 + signal only when the browser is actually signalled).
4. **Manager read:** with a file seeded as `renderer=llvmpipe`, a session detail call moves the
   mark to `software` and records one `renderer.software` history event; with a hardware string,
   the mark is unchanged.
5. **Bare host:** no launcher → unchanged behavior (`renderer: unverified`), no new code path.

## Rejected

- **Parsing Chromium's stderr** — measured empty at every verbosity tried; a parse that never
  matches would mark every session unverified while looking implemented.
- **CDP/DevTools or any automation protocol** — the project's core prohibition (01), and the
  fingerprint the renderer policy exists to avoid.
- **A shell script over `hyprctl`** — no exit-status/signal contract, no atomic 0600 write, and
  nothing a test can exercise without a compositor.
- **The manager injecting the probe page** — the launcher is the documented route; a manager-side
  browser launch would be a second, undocumented path.

## Open

- **05 warning field vs mark flip** — the recommendation above; peer's call if a spoken warning is
  wanted in the status payload.
- **Probe page vs `data:` URL** — a file under the runtime dir is inspectable and removed after
  capture; a data URL keeps the directory cleaner but hides the page from an operator debugging it.
- **Multi-window sessions** — the poll accepts the first client whose title carries the prefix; a
  pre-existing window with that exact prefix is implausible, and the probe page is unique per run.
