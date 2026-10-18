# Packaging — bare-host install and runbook

Assets for two channels (09): the bare-host shape — the manager as a systemd **user** service —
and the container shape, whose split, package set and seeding rule are pinned in 09 and
`p5/image/README.md`. The unit below is the bare-host channel; the image `CMD` is the container
channel, not a replacement for it.

## Install the binary

From a signed release (09, C2's primary channel):

    packaging/release/install.sh --version 0.1.0

It fetches the release manifest and verifies it with `minisign` before it reads any digest,
installs to `~/.local/bin` (`--prefix` overrides), prints the version/protocol/signer receipt,
then runs `daiorama doctor` as an advisory preflight (`--client-only` skips it; `--base` names a
mirror or a local directory). Until the first operator-signed release the trust list ships
empty, so the script refuses `no trusted release key provisioned` — in that window build from
the checkout:

    cargo build --release
    install -Dm755 target/release/daiorama ~/.local/bin/daiorama

The unit below expects `~/.local/bin/daiorama`. A system-wide binary in
`/usr/local/bin` works too — adjust `ExecStart` (see *Adjusting the unit*).

Then the first-run sequence (10, D1):

    daiorama doctor
    daiorama config setup
    daiorama config scope create dashboard --template dashboard   # only for a dashboard connection
    daiorama new

`doctor` names the exact package for anything missing (Hyprland, wayvnc, a
browser). It does not install them.

The per-distribution commands behind those names live in
[`install-commands.md`](install-commands.md) — the maintained table 09 (C2) names: a version for
every row, explicit "not packaged / out of range" rows, and the date the rows were checked.
`doctor`'s in-binary rows come from it, and the build fails if the two disagree.

## Manager service (systemd user)

    install -Dm644 packaging/systemd/daiorama@.service ~/.config/systemd/user/daiorama@.service
    systemctl --user daemon-reload
    systemctl --user enable --now daiorama@default
    systemctl --user status daiorama@default
    journalctl --user -u daiorama@default -f

The unit runs `daiorama serve --instance %i` — the attached foreground form,
never the auto-estimate (10). `KillMode=control-group` means stopping the unit
sweeps the manager's whole session tree; `Delegate=yes` gives the manager the
cgroup subtree its per-session limits need (06).

Sessions do not survive a manager restart or a unit stop. That is the documented
tradeoff (01, 10), not a fault.

### Headless hosts: enable lingering

A user unit only runs while that user's systemd manager is running. On a headless
box nobody logs in graphically, so enable lingering once:

    loginctl enable-linger "$USER"

Without it, a reboot starts no user manager and `daiorama@default` stays down
(and with it every session) until someone logs in.

### Instances

One unit instance per manager instance:

    systemctl --user enable --now daiorama@work

`%i` reaches the binary as `--instance work`, so config, data, and the socket
all follow 08. Give each instance its own listening ports (04 defaults collide
otherwise):

    daiorama --instance work config set manager.bind 127.0.0.1:8766
    daiorama --instance work config set view.port_base 6180

### Adjusting the unit

    systemctl --user edit daiorama@default

A drop-in overrides single directives without editing the shipped file (a
system-wide binary, a different data root). Prefer a drop-in over a copy: the
shipped unit stays the reference and upgrades cleanly.

Do not put secrets in the unit. The instance token lives in
`<data root>/token` (0600, 07) and scope secrets in `<data root>/scope-tokens/`;
an `Environment=DAIORAMA_TOKEN=…` line would put a credential in a readable
unit file and in `systemctl show`.

## First check after enable

    systemctl --user is-active daiorama@default
    daiorama status
    daiorama list

`status` prints state, mode, pid, socket path, versions, and session counts (04).
A unit that is `active` while `status` answers `manager unreachable` means the
process died before binding — read the journal, not `status`.

## Upgrade

Bare host (10, C3):

    daiorama drain --wait
    daiorama daemon end --force
    install -Dm755 daiorama ~/.local/bin/daiorama
    systemctl --user restart daiorama@default
    daiorama status

Drain first so live sessions finish what they can; `--force` drops the rest and
says so. With this unit, `systemctl --user stop`/`restart` is equivalent to
`daiorama daemon end --force` for the unit's tree.

## Backups

- config + scopes: `~/.config/daiorama/<instance>/`
- data: `~/.local/share/daiorama/<instance>/` — token, history, media, modules,
  noVNC assets

The token file is a credential: back it up like one, never in a shared archive
(10).

## View assets (noVNC)

The view front serves a static noVNC tree from `<data root>/novnc` and refuses
with `view_unavailable` until it exists (07). Stage the pinned tree once per
instance, before the first `daiorama view`:

    packaging/novnc/fetch.sh ~/.local/share/daiorama/default/novnc

The script fetches noVNC v1.7.0 from the pinned URL, checks the sha256 before
unpacking anything, and refuses a destination that is not a directory path or
that already holds a non-noVNC tree. Re-running it against an existing noVNC
tree replaces that tree (an upgrade), staged through a temp dir so the front
never serves a half-extracted copy. For another instance use its own data root
(08). An offline host passes the tarball as the second argument:

    packaging/novnc/fetch.sh ~/.local/share/daiorama/default/novnc ./noVNC-1.7.0.tar.gz

`daiorama doctor` reports the tree under `assets:` — installed at the path, or
the front's refusal plus this command. The tree is data, not a build input: it
survives upgrades and belongs in a backup (above), and a container needs it
staged into the persisted `/data` mount, not baked into an ephemeral layer.

## Container images

Two artifacts (09, *Image split*): **`daiorama`** — core substrate, no default command — and
**`daiorama-desktop`** — core plus pinned Chromium, the session launcher at
`/usr/local/bin/daiorama-browser`, and `CMD ["daiorama","serve"]`. The Containerfiles build on
the reference box; CI here has no podman, so their PR carries the build and the live legs.

The desktop image carries the pinned noVNC tree once at `/usr/share/daiorama/novnc`.
`packaging/container/entrypoint.sh` seeds it into `<data root>/daiorama/default/novnc` only when `vnc.html` is
absent, then `exec`s the command — the tree stays on the persisted volume, and an image upgrade
never swaps it under a session. The image sets `ENV XDG_DATA_HOME=/data`, so the manager
and the entrypoint resolve the same data root; without it the seed lands somewhere the
binary never reads. Mount `/data` and `/config` at 08's container equivalents; the
token and scope secrets live in `/data`, so back that volume up like a credential.

The Containerfiles, the Quadlet unit and the compose snippets are the shipped container channel;
the entrypoint is tested beside them so a broken seed rule fails before an image exists.

### Build the core image

The base digest is a required build argument and is never defaulted (09): read the current one
for the tag, then build. There is no podman inside the agent's own container; this runs on the
host or the reference box.

    podman pull -q docker.io/library/debian:sid-slim
    DIGEST=$(podman image inspect --format '{{.Digest}}' docker.io/library/debian:sid-slim)
    podman build --build-arg DEBIAN_SID_DIGEST="$DIGEST" \
      -f packaging/container/Containerfile.core -t daiorama:dev .

The core image has no default command — it is the substrate every shape composes on.

### Build the desktop image

The desktop artifact builds **on the core image**, so build (or pull) core first. `CORE_IMAGE` is
a required build argument, like the base digest: a default tag goes stale silently, and the
release manifest records the exact core reference.

    podman build --build-arg DEBIAN_SID_DIGEST="$DIGEST" \
      --build-arg CORE_IMAGE=localhost/daiorama:dev \
      -f packaging/container/Containerfile.desktop -t localhost/daiorama-desktop:dev .

For an offline build, place the pinned noVNC tarball in `packaging/novnc/` and add
`--build-arg NOVNC_TARBALL=<file>`; `fetch.sh` verifies the same sha256 either way.

It adds pinned Chromium and the session launcher at `/usr/local/bin/daiorama-browser` (built as a
second bin of the root crate in a launcher-only stage), and its `CMD ["daiorama","serve"]` is the
one documented exception to the no-default-command rule. A browser started in the core image
alone is unrouted and the session reads `renderer: unverified`.

### Run it

One compose file per shape (09). The image reference is digest-pinned at use, and compose
refuses to start while it is unset:

    export DAIORAMA_IMAGE=localhost/daiorama@sha256:…
    export DAIORAMA_CONFIG=~/.config/daiorama-container
    export DAIORAMA_DATA=~/.local/share/daiorama-container

    packaging/compose/manager-only.yml        manager exposing the TCP surface (8756)
    packaging/compose/manager-and-bridge.yml  manager plus network bridge on one private network
    packaging/compose/bridge-remote.yml       bare bridge to a remote manager (8757)

    podman-compose -f packaging/compose/manager-only.yml up -d

A published manager port needs `manager.bind` set inside the mounted config volume
(`daiorama config set manager.bind 0.0.0.0:8756`); without it the manager answers on the unix
socket only. The bridge shapes read `DAIORAMA_TOKEN`, `DAIORAMA_SCOPE` and
`DAIORAMA_SCOPE_TOKEN` from `bridge.env` beside the compose file (04, 07). Per-session limits
want a writable, delegated cgroupfs; the snippets mount it and say where to drop it for the
`limits.mode off` downgrade (06).

### Quadlet

For hosts that supervise containers with systemd, the same core image as a user Quadlet:

    install -Dm644 packaging/quadlet/daiorama.container ~/.config/containers/systemd/daiorama.container
    systemctl --user daemon-reload
    systemctl --user start daiorama

`loginctl enable-linger` applies here too. Edit the unit's `Image=` to the digest-pinned
reference and its two `Volume=` paths to the host directories you want persisted; the shipped
paths under `~/.local/share/daiorama-container/` are the defaults, not a requirement.

## Troubleshooting

| symptom | check |
|---|---|
| unit failed; `status` unreachable | `journalctl --user -u daiorama@default -n 100` — the startup line names the bind address and any refusal |
| `Hyprland {found} is outside the supported range {min}-{max}.` | install a supported Hyprland; `doctor` names the package (09, C5) |
| sessions die immediately | `daiorama doctor` — a session needs a working renderer and wayvnc, not just Hyprland |
| cgroup limits refused | the user manager must delegate controllers; if the host cannot, `config set limits.mode off` (06) and the manager reports limits off |
| view answers `view assets not installed (expected <data>/novnc)` | the noVNC tree is missing at `<data root>/novnc`; stage it once with `packaging/novnc/fetch.sh` (*View assets*) |
| another manager answers | sockets are per instance under `$XDG_RUNTIME_DIR/daiorama/<instance>/`; `daiorama status` prints the path it used |

Installer prerequisites: `curl` (download path only), `tar`, `sha256sum`. A missing tool is refused up front, not mid-run.
