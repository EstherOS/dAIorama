# Image split — pre-read (P5)

Two container artifacts, one Containerfile each. This note pins what they contain, what stays
outside an image (data), and which surface each shape carries, so the first Containerfile is a
build step rather than a design argument. It exists because the 2026-10-17 ruling moved the
image split out of the packaging note's prose and into its own row: the notes below are the
decisions that row builds against.

Basis: 09's container section, 06/07/08/10 for what the binary reads and serves, and the P1 L1
image that already ran the whole chain (`p1/l1/Containerfile`, debian sid + hyprland 0.56.2 +
mutter parent + wayvnc + mesa) — the recipe is proven on the reference box, not invented here.

## Artifacts

| artifact | base | carries | `CMD` |
|---|---|---|---|
| `daiorama` (core) | `debian:sid-slim`, digest-pinned | compositor stack, the binary, capture/view tools, fonts/locale, D-Bus/Xwayland | none — substrate only |
| `daiorama-desktop` | `FROM daiorama:<version>` | core + pinned Chromium + the session launcher | `["daiorama","serve"]` |

The desktop image's `CMD` is the documented exception to 09's "no default CMD", and it is
scoped there: the core image stays a substrate an operator composes onto, the desktop image is
the opinionated run-it-now artifact. The systemd unit in `packaging/systemd/` is the bare-host
channel; the image `CMD` is the container channel — two channels, not alternatives (the
runbook states this so neither reads as the other's leftover).

## What the image must carry (and why)

The binary's shipped defaults are external commands, so the package list follows the code, not
the aspirational prose:

- `hyprland` (0.56.0–0.56.9; sid carries 0.56.2) and `mutter` as the invisible parent — the L1
  nested shape (03/09).
- `wayvnc` — `view.wayvnc_command` defaults to `["wayvnc"]` (08).
- `grim` and `wf-recorder` — `capture.screenshot_command` / `capture.clip_command` default to
  `["grim","-t","png"]` / `["wf-recorder","--no-damage","-f"]` (08, `src/config/mod.rs`).
  Capture is argv-configured; an image without these starts fine and fails at the first capture.
- `mesa` userspace (`libgl1-mesa-dri`, `libegl1`, `libgles2`, `libgbm1`), `wayland-utils`,
  `xwayland`, `dbus`, `seatd`, `fontconfig` + `fonts-dejavu-core`, `ca-certificates`.
- **Not** carried: a terminal, an app suite, `wtype`/`wlrctl` (the L1 harness's injection
  stand-ins — the product's input path is in-binary by design), and any browser in the core.

Trixie has no Hyprland package (verified in P1), which is why the base is sid — a rolling base,
so the Containerfile pins it by digest and the build records the snapshot date in the release
manifest (C2) rather than pretending `sid-slim` is a version.

## Data, never an image layer

- **noVNC tree.** Staged data at `<data>/novnc` (07/09). The desktop image ships the pinned tree
  once at `/usr/share/daiorama/novnc` (a read-only seed), and `packaging/container/entrypoint.sh`
  copies it into `<data>/daiorama/default/novnc` (`XDG_DATA_HOME=/data` in the image) **only when the entry point is absent**, then `exec`s the
  command. A container recreate therefore keeps the tree on the volume; an operator can still
  stage or upgrade it with `packaging/novnc/fetch.sh` (the script's re-run replaces the tree).
  The seed is a copy source, not a served path — a `/usr/share` tree served directly would make
  an image upgrade silently change what the front serves mid-session.
- **Browser profile.** Chromium's profile lives under `<data>` (one persisted volume covers
  config + data per 08/09); no separate profile volume. This is the row the withdrawn Firefox
  proposal would have split per browser; with Chromium pinned there is one path.
- **Session state** gets no volume (01/09: disposable by design).

## Launcher path

06 requires browser launches to go through a session launcher "shipped in the image at a fixed
path". The path is pinned as **`/usr/local/bin/daiorama-browser`** (desktop image only): it is
the documented launch route, it captures the browser's own rendered-GL string into the
manager-readable file, and the bare-host channel simply has no such binary (an unrouted browser
stays `renderer: unverified`, 06). Implementing the launcher is runtime-side work; this note
only fixes the name and the image that carries it.

## Quadlet and compose

- The Quadlet unit is a thin wrapper over the core image with the spike's sandbox flags
  (`--cap-drop=ALL`, `--security-opt=no-new-privileges`, render node via `--device
  /dev/dri/renderD128`, `--group-add keep-groups`, `-v <data>:/data`, `-v <config>:/config`,
  `PublishPort=` for the bridge/manager and the view range) — it is the container channel's
  supervision, not a second systemd design; the bare-host unit keeps `serve --instance %i`.
- Compose snippets ship the four shapes 09 names (bare bridge → remote manager; full local
  stack; manager-only; harness + desktop sharing one image), digest-pinned, each with the
  cgroupfs delegation line the spike validated and the `limits.mode = off` comment for hosts
  that cannot delegate.

## Verification (reference box — the build leg)

This box has no podman, so the image is built and exercised on the reference box; the PR that
lands the Containerfiles must carry that evidence, not a local build claim:

1. Both images build from a clean cache; `daiorama doctor` inside the core image reports
   Hyprland in range, wayvnc present, containment delegated or the off-mode refusal.
2. `serve` in the core image with a render node creates a session, `screenshot` returns an
   image block (grim present), `get_view` answers and the noVNC front serves after the
   entrypoint seeds `<data>/daiorama/default/novnc`, `stop_view`/`close` leave no residue.
3. The desktop image's `CMD` starts the manager with no arguments; the launcher path exists and
   the renderer mark is `verified` for a launched Chromium session.
4. A recreate of the container: profile and the seeded view tree survive; session state does not.
5. Entrypoint seeding has an in-process test here (absent → copied; present → untouched; no
   seed → one line and `exec`, never a hard failure).

## Open items (named, not implied)

- **Digest pinning (decided):** a required `--build-arg DEBIAN_SID_DIGEST`; **no checked-in
  default** — a default goes stale silently, and the release manifest records the digest a build
  actually used. The runbook carries the one-liner that reads it for the tag.
- **Desktop Containerfile:** lands with the launcher. The desktop artifact copies the launcher to
  `/usr/local/bin/daiorama-browser`, so a Containerfile naming that path before the file exists
  ships a browser route that cannot work. The core artifact is
  `packaging/container/Containerfile.core`; compose shapes and the Quadlet unit wrap it.
- **`doctor` coverage:** it checks `hyprland`, `wayvnc`, and the asset tree — not `mutter`,
  `grim`, `wf-recorder` or the browser. A missing capture tool surfaces at the first capture,
  not in `doctor`. Adding those checks is a diagnostics slice, tracked separately.
- **Image size:** core is a compositor stack; measure after the first build and record it in
  the release notes rather than promising a number now.
- **Launcher implementation** (runtime side) and the browser-side renderer re-check file name
  remain 06's open surface, not this row's.

## Rejected

- **One monolith image.** 09's split exists because a prebuilt download does not shrink with
  features: an operator running only the bridge should not pull a browser and a terminal.
- **A terminal in core.** It is an app, not substrate; a `-debug` variant or a sidecar is the
  documented route if an operator wants one.
- **Baking the noVNC tree into a served layer.** Recreate loses it or silently changes what the
  front serves on an image bump; the seed-copy keeps one persisted path.
- **Firefox ESR as the desktop browser.** Withdrawn on 2026-10-17: 09's dependency table and
  11 C5 pin Chromium, and a browser swap is a spec change plus a manifest fact, not an
  implementation detail.
