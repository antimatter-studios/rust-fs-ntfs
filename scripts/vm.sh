#!/usr/bin/env bash
# vm.sh status|up|down|exec -- drive the Windows test VM: fs-windows-test-harness's
# persistent Windows ARM64 guest under QEMU (KVM on Linux ARM64, HVF on Apple
# Silicon).
#
# Called by the chore vm:* tasks, which own WHEN each runs (`deps: [vm:up]`);
# this file owns HOW. Every command is the harness's scripts/local-vm.py, run
# in place from the sibling `chore siblings` keeps at HARNESS_REF. Nothing of
# it is copied here, so the harness can change how it runs the guest without
# this repository chasing it.
#
#   status   what the guest is doing, as local-vm.py reports it
#   up       start the guest if it is not running (a running one is reused,
#            with the same boot and disk), then wait until it answers SSH
#   down     ask Windows to shut down over SSH, and wait for QEMU to exit
#   exec -- COMMAND...
#            run COMMAND on this host with the guest's ssh and scp first on
#            PATH. Inside it, VM_HOST=fswth-local reaches the guest with its
#            own key and known_hosts, and no other run can use the guest
#            until COMMAND exits. Every task that reaches the VM runs here.
#
# Settings come from .test-env (loaded by chore's dotenv, or by this script
# when run directly):
#   VM_STATE   the guest's state directory, when it is not the harness's
#              default (~/.local/share/fs-windows-test-harness/arm64)
#
# Setting the guest up: docs/qemu-vm.md in fs-windows-test-harness, then
# scripts/setup-windows-vm.sh through `vm.sh exec`.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HARNESS="$REPO/../fs-windows-test-harness"

if [ -z "${VM_STATE:-}" ] && [ -f "$REPO/.test-env" ]; then
    # Run directly rather than through chore: read the same file.
    while IFS= read -r line; do
        line="${line#export }"
        case "$line" in VM_STATE=*)
            val="${line#*=}"; val="${val#\"}"; val="${val%\"}"
            export "VM_STATE=$val" ;;
        esac
    done < "$REPO/.test-env"
fi

fail() { printf 'vm: %s\n' "$1" >&2; shift; for l in "$@"; do printf '    %s\n' "$l" >&2; done; exit 1; }

[ -f "$HARNESS/scripts/local-vm.py" ] ||
    fail "no scripts/local-vm.py in $HARNESS" "run \`chore siblings\` to check out fs-windows-test-harness at HARNESS_REF"

local_vm() {
    python3 "$HARNESS/scripts/local-vm.py" ${VM_STATE:+--state "$VM_STATE"} "$@"
}

case "${1:-}" in
    status) local_vm status ;;
    # The guest answers SSH 23-33 s after a cold boot on a Pi (the harness's
    # validation reports), so ten minutes is a guest that is not coming up.
    up)     local_vm up && local_vm wait --timeout 600 ;;
    down)   local_vm down ;;
    exec)
        shift
        [ "${1:-}" = "--" ] && shift
        [ "$#" -gt 0 ] || fail "exec needs a command: vm.sh exec -- COMMAND..."
        local_vm exec -- "$@" ;;
    *)      fail "usage: vm.sh status|up|down|exec -- COMMAND..." ;;
esac
