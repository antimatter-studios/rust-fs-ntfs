#!/usr/bin/env bash
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive

case "${FLTH_KERNEL_ARCH:?}:${FLTH_KERNEL_RELEASE:?}" in
    x86_64:6.12.95+deb12-amd64 | aarch64:6.12.95+deb12-arm64) ;;
    *) echo "ntfs3 profile: unsupported architecture/release: $FLTH_KERNEL_ARCH:$FLTH_KERNEL_RELEASE" >&2; exit 1 ;;
esac

echo 'deb https://deb.debian.org/debian bookworm-backports main' \
    >/etc/apt/sources.list.d/bookworm-backports.list
apt-get update -qq
apt-get install -y -qq "linux-image-$FLTH_KERNEL_RELEASE=6.12.95-1~bpo12+1"
