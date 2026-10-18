#!/usr/bin/env python3
"""C2 install-row live leg: what the real `daiorama doctor` prints per release.

The unit tests pin the resolver and the table against each other. This probe drives the
shipped binary instead: it rewrites `/etc/os-release` for every row the table carries, runs
`doctor`, and reads the operator-visible line.

Assertions per case:
  [H] the Hyprland line names the row's command (or the no-package rendering);
  [V] the wayvnc line names the row's command (or the no-package string);
  [E] every printed command is executable as printed -- no annotation, no parentheses, no
      trailing comma (the 09 C2 contract, broken once by an inline version note);
  [T] every printed command appears verbatim in `packaging/install-commands.md` -- the binary
      may not carry a command the maintained table does not.

Usage: probe-install-rows.py <daiorama-binary> <repo-root>
Prints [OK]/[FAIL] per assertion plus FAILS=<n>; exit code = FAILS (capped at 120).
"""
import os
import re
import shutil
import subprocess
import sys

BIN = sys.argv[1]
ROOT = sys.argv[2]
TABLE = os.path.join(ROOT, "packaging", "install-commands.md")
OS_RELEASE = "/etc/os-release"

HYPR_NOT = "not packaged for this distribution; see the release notes for a COPR/upstream build"
WAYVNC_NOT = "not packaged for this distribution; use the container image"

APT_HYPR = "sudo apt install hyprland"
APT_WAYVNC = "sudo apt install wayvnc"
PACMAN_HYPR = "sudo pacman -S hyprland"
PACMAN_WAYVNC = "sudo pacman -S wayvnc"
DNF_WAYVNC = "sudo dnf install wayvnc"

# (ID, VERSION_CODENAME, expected Hyprland command, expected wayvnc command)
CASES = [
    ("arch", None, PACMAN_HYPR, PACMAN_WAYVNC),
    ("fedora", None, HYPR_NOT, DNF_WAYVNC),
    ("debian", "sid", APT_HYPR, APT_WAYVNC),
    ("debian", "forky", APT_HYPR, APT_WAYVNC),
    ("debian", "trixie", HYPR_NOT, APT_WAYVNC),
    ("debian", "bookworm", HYPR_NOT, WAYVNC_NOT),
    ("ubuntu", "resolute", HYPR_NOT, APT_WAYVNC),
    ("ubuntu", "noble", HYPR_NOT, APT_WAYVNC),
    ("ubuntu", "jammy", HYPR_NOT, WAYVNC_NOT),
    ("alpine", None, HYPR_NOT, WAYVNC_NOT),
]

fails = 0
oks = 0
table = open(TABLE, encoding="utf-8").read()


def check(ok, label, detail=""):
    global fails, oks
    if ok:
        oks += 1
        print(f"[OK] {label}")
    else:
        fails += 1
        print(f"[FAIL] {label}{(' -- ' + detail) if detail else ''}")


def write_os_release(os_id, codename):
    lines = [f'ID={os_id}\n']
    if codename:
        lines.append(f'VERSION_CODENAME={codename}\n')
    with open(OS_RELEASE, "w", encoding="utf-8") as fh:
        fh.write("".join(lines))


def line_with(text, needle):
    for line in text.splitlines():
        if needle in line:
            return line
    return None


HYPR_PREFIX = "Hyprland not found. Install it: "
HYPR_SUFFIX = ", or use the container image."
WAY_PREFIX = "wayvnc: missing \u2014 install it: "


def extract_hypr(text):
    line = line_with(text, HYPR_PREFIX)
    if line is None:
        return None
    body = line.split(HYPR_PREFIX, 1)[1]
    if body.endswith(HYPR_SUFFIX):
        body = body[: -len(HYPR_SUFFIX)]
    return body.strip()


def extract_wayvnc(text):
    line = line_with(text, WAY_PREFIX)
    if line is None:
        return None
    return line.split(WAY_PREFIX, 1)[1].strip()


def executable_shape(command):
    bad = ["(", ")", "\u2014", ",", ";", "\n"]
    return all(ch not in command for ch in bad)


backup = shutil.copyfile(OS_RELEASE, "/tmp/os-release.probe-backup")
try:
    for os_id, codename, hypr_expected, way_expected in CASES:
        label = f"{os_id}/{codename or '-'}"
        write_os_release(os_id, codename)
        proc = subprocess.run(
            [BIN, "doctor"], capture_output=True, text=True, timeout=120
        )
        out = proc.stdout + proc.stderr
        hypr = extract_hypr(out)
        way = extract_wayvnc(out)
        check(
            hypr == hypr_expected,
            f"{label} hyprland row",
            f"expected {hypr_expected!r}, got {hypr!r}",
        )
        check(
            way == way_expected,
            f"{label} wayvnc row",
            f"expected {way_expected!r}, got {way!r}",
        )
        for name, got in (("hyprland", hypr), ("wayvnc", way)):
            if got is None:
                continue
            if got in (HYPR_NOT, WAYVNC_NOT):
                continue
            check(
                executable_shape(got),
                f"{label} {name} command is executable as printed",
                repr(got),
            )
            check(
                got in table,
                f"{label} {name} command is carried by install-commands.md",
                repr(got),
            )
finally:
    shutil.copyfile(backup, OS_RELEASE)

print(f"FAILS={min(fails, 120)}")
print(f"OKS={oks}")
sys.exit(min(fails, 120))
