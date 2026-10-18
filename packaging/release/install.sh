#!/bin/sh
# Install a dAIorama release from its GitHub Releases assets (09 C2's primary channel).
#
#   packaging/release/install.sh --version <v> [--target <triple>] [--prefix <dir>]
#                                [--client-only] [--base <url-or-dir>]
#
# The manifest and its detached signature are fetched first and verified with the distribution
# minisign against the keys embedded below; no digest is read before the signature verifies.
# Exit 0 installed; 1 refusal; 2 usage.
set -eu

usage() {
    echo "usage: $0 --version <v> [--target <triple>] [--prefix <dir>] [--client-only] [--base <url|dir>]" >&2
    exit 2
}

version=
target=
prefix=
prefix_given=0
client_only=0
base=

while [ "$#" -gt 0 ]; do
    case $1 in
        --version)
            [ "$#" -ge 2 ] || usage
            version=$2
            shift 2
            ;;
        --target)
            [ "$#" -ge 2 ] || usage
            target=$2
            shift 2
            ;;
        --prefix)
            [ "$#" -ge 2 ] || usage
            prefix=${2%/}
            [ -n "$prefix" ] || { echo "--prefix must not be empty" >&2; exit 1; }
            prefix_given=1
            shift 2
            ;;
        --base)
            [ "$#" -ge 2 ] || usage
            base=${2%/}
            shift 2
            ;;
        --client-only)
            client_only=1
            shift
            ;;
        -h|--help)
            usage
            ;;
        *)
            echo "unknown option $1" >&2
            usage
            ;;
    esac
done

[ -n "$version" ] || usage
# The version is a path/URL component: only semver shapes may reach them.
if ! printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$'; then
    echo "invalid version \"$version\"; expected semver (e.g. 0.1.0)" >&2
    exit 1
fi
if [ "$prefix_given" -eq 0 ]; then
    [ -n "${HOME:-}" ] || { echo "HOME is not set; pass --prefix" >&2; exit 1; }
    prefix=$HOME/.local/bin
fi

# The binary set is Linux; macOS uses the Homebrew tap (client-only) and never this script.
case $(uname -s) in
    Linux) ;;
    *)
        echo "install.sh installs Linux binaries; macOS uses the Homebrew tap (client-only)" >&2
        exit 1
        ;;
esac

if [ -z "$target" ]; then
    case $(uname -m) in
        x86_64|amd64) target=x86_64-unknown-linux-gnu ;;
        aarch64|arm64) target=aarch64-unknown-linux-gnu ;;
        *)
            echo "unsupported architecture $(uname -m); pass --target" >&2
            exit 1
            ;;
    esac
fi
# The four triples the release lane builds; anything else is a typo or an unsupported host,
# and it flows into the artifact name and URL, so it is validated before either is built.
case $target in
    x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu|x86_64-unknown-linux-musl|aarch64-unknown-linux-musl) ;;
    *)
        echo "unsupported target \"$target\"; expected a pinned Rust triple" >&2
        exit 1
        ;;
esac

if [ -z "$base" ]; then
    base="https://github.com/ygypt/dAIorama/releases/download/v$version"
fi

work=$(mktemp -d)
place_tmp=
trap 'rm -rf "$work"; [ -z "$place_tmp" ] || rm -f "$place_tmp"' EXIT

# The trust anchors are embedded, never fetched: a list served beside the manifest would let a
# hostile mirror name its own key. This block is kept byte-identical to
# packaging/release/trusted-keys (the reader test fails the build on drift).
# BEGIN TRUSTED KEYS
cat >"$work/trusted-keys" <<'TRUSTED_KEYS'
# Trusted release keys (09 C2): one minisign public key per entry — its "untrusted comment:"
# line followed by the base64 key. Verification refuses with "no trusted release key
# provisioned" while this list is empty; it is never shipped with an example key. The first
# operator-signed release opens a PR adding the public key and its key id, and that PR is the
# trust anchor. Rotation adds the incoming key here beside the outgoing one; removal is a
# deliberate later release.
TRUSTED_KEYS
# END TRUSTED KEYS

entries=0
while IFS= read -r line; do
    case $line in
        "untrusted comment:"*) entries=$((entries + 1)) ;;
    esac
done <"$work/trusted-keys"
if [ "$entries" -eq 0 ]; then
    echo "no trusted release key provisioned" >&2
    exit 1
fi

if ! command -v minisign >/dev/null 2>&1; then
    echo "minisign is required (install the distribution package)" >&2
    exit 1
fi
if ! command -v sha256sum >/dev/null 2>&1; then
    echo "sha256sum is required" >&2
    exit 1
fi

fetch() { # fetch <name> <dest> <cap-bytes>
    cap=$3
    case $base in
        *://*)
            if ! command -v curl >/dev/null 2>&1; then
                echo "curl is required for a remote release base" >&2
                exit 1
            fi
            if ! curl -fsSL --max-filesize "$cap" "$base/$1" -o "$2"; then
                echo "cannot fetch $base/$1" >&2
                exit 1
            fi
            ;;
        *)
            # Refuse before copying: a local mirror that hands over a 4 GiB file should not
            # fill the temp dir just to be rejected afterwards.
            if [ -f "$base/$1" ]; then
                local_size=$(wc -c <"$base/$1" | tr -d ' ')
                if [ "$local_size" -gt "$cap" ]; then
                    echo "release file $1 is larger than $cap bytes" >&2
                    exit 1
                fi
            fi
            if ! cp "$base/$1" "$2" 2>/dev/null; then
                echo "cannot read $base/$1" >&2
                exit 1
            fi
            ;;
    esac
    size=$(wc -c <"$2" | tr -d ' ')
    if [ "$size" -gt "$cap" ]; then
        echo "release file $1 is larger than $cap bytes" >&2
        exit 1
    fi
}

manifest=$work/daiorama-release-$version.json
signature=$manifest.minisig
# Metadata is small by contract (the verifiers bound it at 1 MiB); the artifact is a binary
# and gets the installer's own 128 MiB ceiling, never the metadata cap.
fetch "daiorama-release-$version.json" "$manifest" 1048576
fetch "daiorama-release-$version.json.minisig" "$signature" 1048576

# The signature is the whole trust decision: the exit code is the contract, its output is not
# parsed. Try each anchor; a signature chains to exactly one key id, so at most one succeeds.
signer=
while IFS= read -r line; do
    case $line in
        "untrusted comment:"*)
            if ! IFS= read -r encoded || [ -z "$encoded" ]; then
                echo "trusted key list: public key line is missing" >&2
                exit 1
            fi
            printf '%s\n%s\n' "$line" "$encoded" >"$work/key.pub"
            if minisign -Vqm "$manifest" -x "$signature" -p "$work/key.pub" >/dev/null 2>&1; then
                signer=$(printf '%s\n' "$line" | sed 's/.*[Kk]ey //')
                break
            fi
            ;;
    esac
done <"$work/trusted-keys"

if [ -z "$signer" ]; then
    signature_id=
    if command -v base64 >/dev/null 2>&1 && command -v od >/dev/null 2>&1; then
        encoded_signature=$(sed -n '2p' "$signature")
        signature_id=$(printf '%s' "$encoded_signature" | base64 -d 2>/dev/null \
            | od -An -tx1 -j2 -N8 2>/dev/null \
            | awk '{ for (i = NF; i > 0; i--) printf toupper($i) }')
    fi
    if [ -n "$signature_id" ] && ! grep -qi "key $signature_id" "$work/trusted-keys"; then
        echo "release signed by an untrusted key $signature_id" >&2
    else
        echo "release signature verification failed" >&2
    fi
    exit 1
fi

# Only now may the manifest bytes be read as facts.
value() {
    sed -n "s/.*\"$1\"[[:space:]]*:[[:space:]]*\"\([^\"]*\)\".*/\1/p" "$manifest" | head -n 1
}

format=$(sed -n 's/.*"format"[[:space:]]*:[[:space:]]*\([0-9]*\).*/\1/p' "$manifest" | head -n 1)
if [ "$format" != 1 ]; then
    echo "unsupported release manifest format ${format:-missing}" >&2
    exit 1
fi

manifest_version=$(value version)
protocol=$(value protocol)
if [ -z "$manifest_version" ] || [ -z "$protocol" ]; then
    echo "release manifest is missing version or protocol" >&2
    exit 1
fi
if [ "$manifest_version" != "$version" ]; then
    echo "release manifest version $manifest_version does not match the requested $version" >&2
    exit 1
fi

name=daiorama-$version-$target
rows=$(grep -F "\"name\":\"$name\"" "$manifest" || true)
if [ -z "$rows" ]; then
    echo "no artifact \"$name\" in this release" >&2
    exit 1
fi
rows_count=$(printf '%s\n' "$rows" | wc -l | tr -d ' ')
if [ "$rows_count" -ne 1 ]; then
    echo "duplicate release manifest row $name" >&2
    exit 1
fi

expected=$(printf '%s\n' "$rows" | sed -n 's/.*"sha256":"\([0-9a-f]\{64\}\)".*/\1/p')
kind=$(printf '%s\n' "$rows" | sed -n 's/.*"kind":"\([^"]*\)".*/\1/p')
# The name alone is not the contract: the signed row's target must be the one asked for,
# or a re-signed manifest could hand an aarch64 binary to an x86_64 install.
row_target=$(printf '%s\n' "$rows" | sed -n 's/.*"target":"\([^"]*\)".*/\1/p')
if [ "$row_target" != "$target" ]; then
    echo "release manifest row $name targets \"$row_target\", not \"$target\"" >&2
    exit 1
fi
if [ -z "$expected" ] || [ "$kind" != core ]; then
    echo "release manifest row $name is not a core artifact" >&2
    exit 1
fi

fetch "$name" "$work/$name" 134217728
actual=$(sha256sum "$work/$name" | awk '{ print $1 }')
if [ "$actual" != "$expected" ]; then
    echo "artifact \"$name\" sha256 $actual does not match the manifest row $expected" >&2
    exit 1
fi

mkdir -p "$prefix" || {
    echo "cannot create $prefix" >&2
    exit 1
}
place_tmp=$prefix/.daiorama.tmp.$$
cp "$work/$name" "$place_tmp"
chmod 755 "$place_tmp"
mv "$place_tmp" "$prefix/daiorama"
place_tmp=

printf 'installed daiorama %s (protocol %s) %s\n' "$version" "$protocol" "$target"
printf 'signed by %s\n' "$signer"
printf 'sha256 %s\n' "$actual"

if [ "$client_only" -eq 0 ]; then
    printf 'manager preflight: %s doctor\n' "$prefix/daiorama"
    "$prefix/daiorama" doctor || printf 'doctor reported host gaps above; the binary is installed\n' >&2
else
    printf 'client-only install: CLI and bridge; manager subcommands need Hyprland and wayvnc (doctor names them)\n'
fi
