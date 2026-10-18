#!/bin/bash
# D5 live leg: `daiorama dashboard install`, end to end, on the merged tip.
#
# Real binary, real gzipped tars built here, real data dir. Every refusal is checked for its
# pinned message AND for a module tree that did not move; every success is checked through the
# core exec path (`dashboard connections list` runs the installed module binary for real).
#
# Usage: probe-install.sh <daiorama> <daiorama-dashboard> <label>
set -u
BIN=$(readlink -f "$1")
MOD=$(readlink -f "$2")
LABEL=${3:-run}
ROOT=/tmp/di-$LABEL
rm -rf "$ROOT"
mkdir -p "$ROOT"/config/daiorama/default "$ROOT"/data "$ROOT"/run "$ROOT"/art "$ROOT"/art2
printf '(version: 1, limits: (mode: off))\n' > "$ROOT"/config/daiorama/default/config.ron
export XDG_CONFIG_HOME="$ROOT/config"
export XDG_DATA_HOME="$ROOT/data"
export XDG_RUNTIME_DIR="$ROOT/run"
export TMPDIR="$ROOT"

PARENT="$ROOT/data/daiorama/default/modules/daiorama-dashboard"
fails=0
ok()  { printf '[OK]   %s%s\n' "$1" "${2:+ ($2)}"; }
bad() { fails=$((fails + 1)); printf '[FAIL] %s :: %s\n' "$1" "$2"; }
chk() { if [ "$2" = "$3" ]; then ok "$1" "$3"; else bad "$1" "got '$2', want '$3'"; fi; }
snap() { if [ -d "$PARENT" ]; then (cd "$PARENT" && ls -A | sort | tr '\n' ' '); else printf 'ABSENT'; fi; }
lines() { printf '%s' "$1" | wc -l; }

# expect a refused invocation: non-zero exit, pinned text present, and the named tree state.
# refuse <label> <needle> <expected-after-snapshot> <args...>
refuse() {
    local label=$1 needle=$2 want=$3
    shift 3
    local text after rc
    text=$("$BIN" "$@" 2>&1)
    rc=$?
    after=$(snap)
    if [ "$rc" -eq 0 ]; then bad "$label" "expected a refusal, exited 0: $text"; return; fi
    case "$text" in
        *"$needle"*)
            if [ "$after" = "$want" ]; then
                ok "$label" "$(printf '%s' "$text" | head -1)"
            else
                bad "$label" "tree '$after', want '$want'"
            fi
            ;;
        *) bad "$label" "no '$needle' in: $text" ;;
    esac
}

# ---- artifacts ---------------------------------------------------------------
mkdir -p "$ROOT"/pkg/good/assets
cp "$MOD" "$ROOT"/pkg/good/daiorama-dashboard
chmod 644 "$ROOT"/pkg/good/daiorama-dashboard   # the installer must repair the mode bit
printf '<!doctype html><title>probe</title>\n' > "$ROOT"/pkg/good/assets/index.html
GOOD="$ROOT"/art/daiorama-dashboard-0.1.0-x86_64-unknown-linux-gnu.tar.gz
tar -C "$ROOT"/pkg/good -czf "$GOOD" .
GOOD_SHA=$(sha256sum "$GOOD" | cut -d' ' -f1)

mkdir -p "$ROOT"/pkg/good2/assets
cp "$MOD" "$ROOT"/pkg/good2/daiorama-dashboard
printf 'asset v2\n' > "$ROOT"/pkg/good2/assets/index.html
GOOD2="$ROOT"/art2/daiorama-dashboard-0.1.0-x86_64-unknown-linux-gnu.tar.gz
tar -C "$ROOT"/pkg/good2 -czf "$GOOD2" .
GOOD2_SHA=$(sha256sum "$GOOD2" | cut -d' ' -f1)

# Hostile and malformed artifacts, built with tarfile so the entry names are exact.
python3 - "$ROOT" <<'PY'
import io, os, sys, tarfile, time

root = sys.argv[1]
art = os.path.join(root, "art")
mod = os.path.join(root, "pkg", "good", "daiorama-dashboard")
mod_bytes = open(mod, "rb").read()


def entry(tf, name, data=b"", mode=0o644, kind=tarfile.REGTYPE, link=""):
    info = tarfile.TarInfo(name)
    info.size = len(data) if kind == tarfile.REGTYPE else 0
    info.mode = mode
    info.type = kind
    info.linkname = link
    info.mtime = int(time.time())
    tf.addfile(info, io.BytesIO(data) if kind == tarfile.REGTYPE else None)


# `../escape`: the classic path traversal member.
with tarfile.open(os.path.join(art, "dotdot.tar.gz"), "w:gz") as tf:
    entry(tf, "daiorama-dashboard", mod_bytes, 0o755)
    entry(tf, "../escape", b"escaped\n")

# An absolute member path.
with tarfile.open(os.path.join(art, "absolute.tar.gz"), "w:gz") as tf:
    entry(tf, "daiorama-dashboard", mod_bytes, 0o755)
    entry(tf, "/tmp/di-abs-escape", b"escaped\n")

# A symlink member (the module binary as a link elsewhere).
with tarfile.open(os.path.join(art, "symlink.tar.gz"), "w:gz") as tf:
    entry(tf, "daiorama-dashboard", kind=tarfile.SYMTYPE, link="/bin/sh")

# A hardlink member.
with tarfile.open(os.path.join(art, "hardlink.tar.gz"), "w:gz") as tf:
    entry(tf, "daiorama-dashboard", mod_bytes, 0o755)
    entry(tf, "daiorama-dashboard-link", kind=tarfile.LNKTYPE, link="daiorama-dashboard")

# No module binary at the root.
with tarfile.open(os.path.join(art, "nobinary.tar.gz"), "w:gz") as tf:
    entry(tf, "assets/index.html", b"<html></html>\n")
PY

for a in dotdot absolute symlink hardlink nobinary; do
    eval "SHA_${a}=\$(sha256sum \"\$ROOT/art/$a.tar.gz\" | cut -d' ' -f1)"
done
printf 'not a tarball at all\n' > "$ROOT"/art/plain.txt
PLAIN_SHA=$(sha256sum "$ROOT"/art/plain.txt | cut -d' ' -f1)
truncate -s 129M "$ROOT"/art/huge.tar.gz
HUGE_SHA=$(sha256sum "$ROOT"/art/huge.tar.gz | cut -d' ' -f1)

echo "== [A] pre-install and input-shape refusals =="
chk "module tree absent at a cold data root" "$(snap)" "ABSENT"
refuse "no module: the exec path refuses" "module not installed" "ABSENT" dashboard connections list
refuse "--artifact without --sha256 refuses" "--artifact requires --sha256" "ABSENT" \
    dashboard install --artifact "$GOOD"
refuse "--sha256 without --artifact refuses" "--sha256 applies only with --artifact" "ABSENT" \
    dashboard install --sha256 "$GOOD_SHA"
refuse "neither input refuses" "dashboard install needs --artifact" "ABSENT" \
    dashboard install
refuse "a remote target refuses install before touching disk" "local-instance only" "ABSENT" \
    --remote 127.0.0.1:1 dashboard install --artifact "$GOOD" --sha256 "$GOOD_SHA"

echo "== [B] the checksum gate runs before anything under modules/ =="
refuse "malformed digest refuses" "invalid --sha256" "ABSENT" \
    dashboard install --artifact "$GOOD" --sha256 "nothex"
refuse "wrong 64-hex digest refuses" "artifact checksum mismatch" "ABSENT" \
    dashboard install --artifact "$GOOD" --sha256 "$(printf '0%.0s' $(seq 64))"
refuse "129 MiB artifact refuses on size, before hashing" "artifact too large" "ABSENT" \
    dashboard install --artifact "$ROOT/art/huge.tar.gz" --sha256 "$HUGE_SHA"
# Post-#95 a refused first install removes both tree levels it created, so the expected
# after-state is ABSENT — the empty modules/daiorama-dashboard/ this leg used to tolerate
# was the old nit, and the in-tree contract test pins ABSENT.
refuse "a non-tar artifact refuses" "cannot read artifact" "ABSENT" \
    dashboard install --artifact "$ROOT/art/plain.txt" --sha256 "$PLAIN_SHA" --version 0.1.0
refuse "a directory is not an artifact" "is not a regular file" "ABSENT" \
    dashboard install --artifact "$ROOT/pkg/good" --sha256 "$GOOD_SHA"

echo "== [C] hostile and incomplete members are refused, and nothing escapes =="
refuse "'../escape' member refuses" "artifact entry refused" "ABSENT" \
    dashboard install --artifact "$ROOT/art/dotdot.tar.gz" --sha256 "$SHA_dotdot" --version 0.1.0
[ -e "$ROOT/escape" ] && bad "no file escaped the staging dir" "$ROOT/escape exists" || ok "no file escaped the staging dir"
[ -e "$ROOT/data/daiorama/escape" ] && bad "no file landed under the data root" "escape exists" || ok "no file landed under the data root"
refuse "an absolute member refuses" "artifact entry refused" "ABSENT" \
    dashboard install --artifact "$ROOT/art/absolute.tar.gz" --sha256 "$SHA_absolute" --version 0.1.0
[ -e /tmp/di-abs-escape ] && bad "the absolute member was not written" "/tmp/di-abs-escape exists" || ok "the absolute member was not written"
refuse "a symlink member refuses" "artifact entry refused" "ABSENT" \
    dashboard install --artifact "$ROOT/art/symlink.tar.gz" --sha256 "$SHA_symlink" --version 0.1.0
refuse "a hardlink member refuses" "artifact entry refused" "ABSENT" \
    dashboard install --artifact "$ROOT/art/hardlink.tar.gz" --sha256 "$SHA_hardlink" --version 0.1.0
refuse "an artifact without the module binary refuses" "artifact does not contain" "ABSENT" \
    dashboard install --artifact "$ROOT/art/nobinary.tar.gz" --sha256 "$SHA_nobinary" --version 0.1.0
chk "refusals left no staged or version directory behind" "$(snap)" "ABSENT"

echo "== [D] a real install =="
OUT=$("$BIN" dashboard install --artifact "$GOOD" --sha256 "$GOOD_SHA" 2>&1)
RC=$?
chk "install exits 0" "$RC" "0"
chk "install prints exactly two lines" "$(printf '%s\n' "$OUT" | wc -l)" "2"
chk "line 1 is the version read from the asset name" "$(printf '%s\n' "$OUT" | sed -n 1p)" "0.1.0"
chk "line 2 is the instance directory" "$(printf '%s\n' "$OUT" | sed -n 2p)" "$PARENT/0.1.0"
chk "active names one slug" "$(readlink "$PARENT/active")" "0.1.0"
chk "the installed binary is 0755 although the artifact carried 0644" \
    "$(stat -c '%a' "$PARENT/0.1.0/daiorama-dashboard")" "755"
chk "the rest of the artifact landed beside it" "$(cat "$PARENT/0.1.0/assets/index.html")" \
    "<!doctype html><title>probe</title>"
chk "no .stage-/ .old- leftovers" "$(snap)" "0.1.0 active "
OUT=$("$BIN" dashboard connections list 2>&1)
chk "the core execs the installed module (exit 0)" "$?" "0"
chk "the module answered for real" "$OUT" "no connections"

echo "== [E] replace, second version, and revert =="
refuse "a second install of the same version refuses" "already installed" "0.1.0 active " \
    dashboard install --artifact "$GOOD2" --sha256 "$GOOD2_SHA"
OUT=$("$BIN" dashboard install --artifact "$GOOD2" --sha256 "$GOOD2_SHA" --force 2>&1)
chk "--force replaces the real directory (exit 0)" "$?" "0"
chk "--force keeps active on the same slug" "$(readlink "$PARENT/active")" "0.1.0"
chk "--force landed the new payload" "$(cat "$PARENT/0.1.0/assets/index.html")" "asset v2"
chk "--force leaves no .old- directory" "$(snap)" "0.1.0 active "
chk "the exec path still works after --force" "$("$BIN" dashboard connections list 2>&1)" "no connections"
OUT=$("$BIN" dashboard install --artifact "$GOOD2" --sha256 "$GOOD2_SHA" --version 0.2.0 2>&1)
chk "a second version installs beside the first (exit 0)" "$?" "0"
chk "active switches to the new version" "$(readlink "$PARENT/active")" "0.2.0"
refuse "the core refuses to exec a version-skewed module" "does not match this dashboard" \
    "0.1.0 0.2.0 active " dashboard connections list
OUT=$("$BIN" dashboard install --version 0.1.0 2>&1)
chk "revert takes no digest and exits 0" "$?" "0"
chk "revert prints the version and path" "$(printf '%s\n' "$OUT" | head -1)" "0.1.0"
chk "active is back on 0.1.0" "$(readlink "$PARENT/active")" "0.1.0"
chk "the exec path works again with no restart" "$("$BIN" dashboard connections list 2>&1)" "no connections"
refuse "reverting to a version that is not installed refuses" "is not installed" \
    "0.1.0 0.2.0 active " dashboard install --version 9.9.9

echo "== [F] version-directory safety =="
mkdir -p "$ROOT"/outside
ln -s "$ROOT/outside" "$PARENT/9.9.9"
refuse "a symlinked version directory is never replaced" "is not a directory" "0.1.0 0.2.0 9.9.9 active " \
    dashboard install --artifact "$GOOD2" --sha256 "$GOOD2_SHA" --version 9.9.9 --force
chk "the symlink is intact (still points outside)" "$(readlink "$PARENT/9.9.9")" "$ROOT/outside"
refuse "--version refuses a path component" "invalid module version" "0.1.0 0.2.0 9.9.9 active " \
    dashboard install --artifact "$GOOD2" --sha256 "$GOOD2_SHA" --version "../escape"
refuse "--version refuses a dot" "invalid module version" "0.1.0 0.2.0 9.9.9 active " \
    dashboard install --version "."

echo "== [G] one more real exec after every refusal =="
chk "active unchanged" "$(readlink "$PARENT/active")" "0.1.0"
chk "the module still answers" "$("$BIN" dashboard connections list 2>&1)" "no connections"

echo "FAILS=$fails"
[ "$fails" -eq 0 ] && echo "VERDICT=PASS" || echo "VERDICT=FAIL"
exit "$fails"
