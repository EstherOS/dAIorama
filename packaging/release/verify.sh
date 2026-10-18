#!/bin/sh
# Verify a signed release manifest with the distribution's minisign (09 C2). This is the
# independent second implementation used by release automation; the binary verifies in-process
# with a pinned pure-Rust crate. Both must agree on the same bytes.
#
#   packaging/release/verify.sh <manifest> <signature> [artifact]
#
# Trust anchors: packaging/release/trusted-keys, one minisign public key per entry. The list ships
# empty and this script refuses rather than trusting anything until an operator adds a key.
# Exit 0 verified; 1 refusal; 2 usage.
set -eu

if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
    echo "usage: $0 <manifest> <signature> [artifact]" >&2
    exit 2
fi

manifest=$1
signature=$2
artifact=${3:-}
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
trusted_keys=$here/trusted-keys

if [ ! -f "$manifest" ]; then
    echo "cannot read $manifest" >&2
    exit 1
fi
if [ ! -f "$signature" ]; then
    echo "cannot read $signature" >&2
    exit 1
fi
if [ ! -f "$trusted_keys" ]; then
    echo "no trusted release key provisioned" >&2
    exit 1
fi

# Count key entries first: an empty list is its own refusal, not a failed signature. This is
# checked before the minisign dependency so an unprovisioned install refuses for the right reason.
entries=0
while IFS= read -r line; do
    case $line in
        "untrusted comment:"*) entries=$((entries + 1)) ;;
    esac
done <"$trusted_keys"
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

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# Try each anchor. A signature chains to exactly one key id, so at most one verify succeeds.
signer=
while IFS= read -r line; do
    case $line in
        "untrusted comment:"*)
            if ! IFS= read -r encoded || [ -z "$encoded" ]; then
                echo "trusted key file: public key line is missing" >&2
                exit 1
            fi
            printf '%s\n%s\n' "$line" "$encoded" >"$work/key.pub"
            if minisign -Vqm "$manifest" -x "$signature" -p "$work/key.pub" >/dev/null 2>&1; then
                signer=$(printf '%s\n' "$line" | sed 's/.*[Kk]ey //')
                break
            fi
            ;;
    esac
done <"$trusted_keys"

if [ -z "$signer" ]; then
    # Match the binary's split: known key id + bad signature is a verification failure; an id not
    # in the list is an untrusted signer. The id lives at bytes 2..10 of the decoded signature,
    # printed little-endian by minisign.
    signature_id=
    if command -v base64 >/dev/null 2>&1 && command -v od >/dev/null 2>&1; then
        encoded_signature=$(sed -n '2p' "$signature")
        signature_id=$(printf '%s' "$encoded_signature" | base64 -d 2>/dev/null \
            | od -An -tx1 -j2 -N8 2>/dev/null \
            | awk '{ for (i = NF; i > 0; i--) printf toupper($i) }')
    fi
    if [ -n "$signature_id" ] && ! grep -qi "key $signature_id" "$trusted_keys"; then
        echo "release signed by an untrusted key $signature_id" >&2
    else
        echo "release signature verification failed" >&2
    fi
    exit 1
fi

value() {
    sed -n "s/.*\"$1\"[[:space:]]*:[[:space:]]*\"\([^\"]*\)\".*/\1/p" "$manifest" | head -n 1
}

format=$(sed -n 's/.*"format"[[:space:]]*:[[:space:]]*\([0-9]*\).*/\1/p' "$manifest" | head -n 1)
if [ "$format" != 1 ]; then
    echo "unsupported release manifest format ${format:-missing}" >&2
    exit 1
fi

version=$(value version)
protocol=$(value protocol)
if [ -z "$version" ] || [ -z "$protocol" ]; then
    echo "release manifest is missing version or protocol" >&2
    exit 1
fi

printf 'manifest %s (protocol %s)\n' "$version" "$protocol"
printf 'signed by %s\n' "$signer"

if [ -n "$artifact" ]; then
    name=$(basename "$artifact")
    row=$(awk -v name="$name" 'index($0, "\"name\"") && index($0, "\"" name "\"")' "$manifest" | head -n 1)
    if [ -z "$row" ]; then
        echo "artifact \"$name\" is not in the manifest" >&2
        exit 1
    fi
    if [ ! -f "$artifact" ]; then
        echo "cannot read $artifact" >&2
        exit 1
    fi
    expected=$(printf '%s\n' "$row" | sed -n 's/.*"sha256"[[:space:]]*:[[:space:]]*"\([0-9a-fA-F]*\)".*/\1/p')
    kind=$(printf '%s\n' "$row" | sed -n 's/.*"kind"[[:space:]]*:[[:space:]]*"\([^\"]*\)".*/\1/p')
    actual=$(sha256sum "$artifact" | awk '{ print $1 }')
    if [ -z "$expected" ] || [ "$actual" != "$expected" ]; then
        echo "artifact \"$name\" sha256 $actual does not match the manifest row ${expected:-missing}" >&2
        exit 1
    fi
    printf 'artifact %s %s sha256 %s\n' "$kind" "$name" "$actual"
fi
