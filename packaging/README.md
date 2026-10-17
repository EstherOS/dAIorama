# Packaging — bare-host install and runbook

Assets for the bare-host shape (09): the manager as a systemd **user** service.
Container assets (image, Quadlet unit, compose snippets) are the next packaging
slice; 09 carries their design.

## Install the binary

Until the first signed release (09, C2), build from the checkout:

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

## Troubleshooting

| symptom | check |
|---|---|
| unit failed; `status` unreachable | `journalctl --user -u daiorama@default -n 100` — the startup line names the bind address and any refusal |
| `Hyprland {found} is outside the supported range {min}-{max}.` | install a supported Hyprland; `doctor` names the package (09, C5) |
| sessions die immediately | `daiorama doctor` — a session needs a working renderer and wayvnc, not just Hyprland |
| cgroup limits refused | the user manager must delegate controllers; if the host cannot, `config set limits.mode off` (06) and the manager reports limits off |
Installer prerequisites: `curl` (download path only), `tar`, `sha256sum`. A missing tool is refused up front, not mid-run.

| view answers `view assets not installed (expected <data>/novnc)` | the noVNC tree is missing at `<data root>/novnc`; stage it once with `packaging/novnc/fetch.sh` (*View assets*) |
| another manager answers | sockets are per instance under `$XDG_RUNTIME_DIR/daiorama/<instance>/`; `daiorama status` prints the path it used |
