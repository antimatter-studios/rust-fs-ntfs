#!/usr/bin/env bash
# The Linux test VM's setup, run as root INSIDE the guest by
# fs-linux-test-harness once per disposable shared-guest boot. The ntfs3
# run profile prepares the exact kernel before this consumer joins.
# Check the running kernel, never the newest installed one (#1288).
set -euo pipefail
running="$(uname -r)"
if ! modinfo -k "$running" ntfs3 >/dev/null 2>&1 || ! modprobe ntfs3; then
    echo "vm-setup: running kernel $running has no loadable ntfs3; select the ntfs3 run profile" >&2
    exit 1
fi
echo "vm-setup: running kernel $running has ntfs3"
