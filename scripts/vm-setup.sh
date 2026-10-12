#!/usr/bin/env bash
# The Linux test VM's setup, run as root INSIDE the guest by
# fs-linux-test-harness on a provisioning boot, whenever this file changes.
# It is the only thing that writes the guest's disk: every later boot runs
# on a throwaway overlay of it.
#
# WHAT THE KERNEL ORACLE NEEDS is a kernel with ntfs3 (#486). Debian 12's
# own kernels are built without it (`# CONFIG_NTFS3_FS is not set`), and
# the harness's box runs one of them. bookworm-backports' generic image is
# built with ntfs3 as a module, so it is installed here. The harness stops
# the provisioning boot when this script ends and boots the next from this
# disk, so the next boot comes up on the newest kernel installed, with no
# reboot here.
#
# A kernel without ntfs3 is refused HERE, at provisioning, naming the
# config it read, rather than in every test that would mount an image.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive

arch="$(dpkg --print-architecture)"
echo 'deb http://deb.debian.org/debian bookworm-backports main' \
    >/etc/apt/sources.list.d/bookworm-backports.list
apt-get update -qq
apt-get install -y -qq -t bookworm-backports "linux-image-$arch" >/dev/null

newest="$(find /boot -maxdepth 1 -name 'config-*' | sort -V | tail -n 1)"
if ! grep -qE '^CONFIG_NTFS3_FS=(m|y)$' "$newest"; then
    echo "vm-setup: $newest, the newest kernel installed, has no ntfs3:" >&2
    grep -E 'CONFIG_NTFS3_FS' "$newest" >&2 || echo "  (no CONFIG_NTFS3_FS line)" >&2
    exit 1
fi
echo "vm-setup: ${newest#/boot/config-} has ntfs3"
