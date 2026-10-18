# Install-command table

The maintained distro/release → exact command → version table 09 (C2) names. `doctor` resolves
its in-binary rows from here; a command this table does not carry is not one the binary may
print, and vice versa (the reader pin in `tests/packaging.rs` and the doctor unit tests fail on
drift).

Checked **2026-10-18** against the distribution package indexes (links under *Sources*).
Re-check each release — a row is a fact about a moving archive, not a permanent claim.

**Supported Hyprland range: 0.56.0–0.56.9** (04's version check). A packaged version below it is
an "out of range" row, not an install command: `doctor` renders the no-package string instead,
because upgrading past a distribution's archive is not an install.

Where no package exists in range, `doctor` prints exactly:

    not packaged for this distribution; see the release notes for a COPR/upstream build

## Hyprland (the compositor — manager hosts only)

| distribution | release | Hyprland | command |
|---|---|---|---|
| Arch Linux | rolling | 0.56.2-4 | `sudo pacman -S hyprland` |
| Debian | sid / forky (unstable, testing) | 0.56.2+ds-3 | `sudo apt install hyprland` |
| Debian | trixie (13, stable) | none in main; trixie-backports has 0.55.2 (below range) | no-package string |
| Ubuntu | 26.04 LTS (resolute) | 0.53.3 (below range) | no-package string |
| Ubuntu | 24.04 LTS (noble) and older | not packaged | no-package string |
| Fedora | 43 and later | none — orphaned; newest build is f42 0.45.2 (below range) | no-package string |
| Fedora | 42 and older | 0.45.2 (below range) | no-package string |
| anything else | — | unknown | no-package string; use the image |

macOS is client-only (09): the manager and its compositor are Linux-only, so there is no
Hyprland row.

## wayvnc (the view server)

| distribution | release | wayvnc | command |
|---|---|---|---|
| Arch Linux | rolling | 0.10.2-1 | `sudo pacman -S wayvnc` |
| Debian | sid / forky / trixie | 0.9.1-1 | `sudo apt install wayvnc` |
| Ubuntu | 24.04 LTS and later | 0.9.1-1 | `sudo apt install wayvnc` |
| Fedora | 42 and later | packaged | `sudo dnf install wayvnc` |

## Binary channels

The primary channel installs the binary itself, not a dependency. Until the first signed release
lands (the C2 manifest), build from the checkout:

    cargo build --release

Published channels, once releases exist: the GitHub Releases install script (digest-pinned via
the signed manifest), the container image and its compose/Quadlet shapes
(`packaging/README.md`), the Homebrew tap (client-only), and `cargo install` from crates.io.

## Sources

- https://archlinux.org/packages/extra/x86_64/hyprland/ · https://archlinux.org/packages/extra/x86_64/wayvnc/
- https://packages.debian.org/sid/hyprland · https://packages.debian.org/trixie/hyprland · https://packages.debian.org/trixie/wayvnc
- https://packages.ubuntu.com/search?keywords=hyprland&searchon=names&suite=all · https://packages.ubuntu.com/resolute/wayvnc
- https://packages.fedoraproject.org/pkgs/hyprland/hyprland/
