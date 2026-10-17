#!/bin/sh
# Verify the built image artifact, not the Containerfile text (09): the base image's inherited
# command, and a Hyprland that actually executes inside a rootless container.
set -eu

IMG=${1:?usage: verify-image.sh <image>}

CMD=$(podman inspect -f '{{json .Config.Cmd}}' "$IMG")
if [ "$CMD" != "null" ]; then
    echo "refusing: default command present: $CMD" >&2
    exit 1
fi

podman run --rm "$IMG" /usr/bin/Hyprland --version
podman run --rm "$IMG" sh -c '! getcap /usr/bin/Hyprland | grep -q .'
echo "image ok: no default command; Hyprland execs without file capabilities"
