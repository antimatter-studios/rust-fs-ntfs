#!/usr/bin/env bash
# scripts/vm.sh drives the Windows test VM through fs-windows-test-harness's
# scripts/local-vm.py, run in place from the sibling checkout. What it must
# get right is what it hands that script:
#
#   * --state comes BEFORE the subcommand (local-vm.py refuses it after),
#     and only when .test-env names VM_STATE, with a space in it kept whole;
#   * `up` waits for SSH only once the guest started, and never waits on a
#     guest that failed to start;
#   * `exec -- COMMAND...` hands the command over word for word;
#   * a missing harness or an empty exec is a one-line reason, not a trace.
#
# Black-box: the real vm.sh runs in a sandbox, beside a stand-in local-vm.py
# that records its arguments, one call per line, separated by `|`.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/repo/scripts" "$tmp/fs-windows-test-harness/scripts"
ln -s "$ROOT/scripts/vm.sh" "$tmp/repo/scripts/vm.sh"
calls="$tmp/calls"
cat > "$tmp/fs-windows-test-harness/scripts/local-vm.py" <<'EOF'
import os, sys
args = sys.argv[1:]
with open(os.environ["CALLS"], "a") as f:
    f.write("|".join(args) + "\n")
sys.exit(1 if args and args[-1] == "up" and os.environ.get("UP_FAILS") else 0)
EOF

fails=0
bad() { printf 'FAIL %s\n  got: %s\n' "$1" "$2" >&2; fails=$((fails + 1)); }
run() { : > "$calls"; (cd "$tmp" && env -u VM_STATE CALLS="$calls" "$@" bash repo/scripts/vm.sh "${vm_args[@]}") ; }

# ── 1. No VM_STATE: the harness's default state, `up` then `wait` ──────────
vm_args=(up)
run env >/dev/null 2>&1 || bad "up exits 0 when the guest starts" "$?"
got="$(cat "$calls")"
[ "$got" = $'up\nwait|--timeout|600' ] || bad "up starts the guest, then waits for SSH" "$got"

# ── 2. VM_STATE from .test-env, quoted and with a space ────────────────────
printf 'export VM_STATE="%s"\n' "$tmp/guest state" > "$tmp/repo/.test-env"
vm_args=(status)
run env >/dev/null 2>&1 || bad "status exits 0" "$?"
got="$(cat "$calls")"
[ "$got" = "--state|$tmp/guest state|status" ] || bad "--state comes first and stays one word" "$got"
rm "$tmp/repo/.test-env"

# ── 3. A guest that fails to start is not waited on ────────────────────────
vm_args=(up)
if run env UP_FAILS=1 >/dev/null 2>&1; then bad "up fails when the guest does not start" "exit 0"; fi
got="$(cat "$calls")"
[ "$got" = "up" ] || bad "no wait after a failed start" "$got"

# ── 4. exec hands the command over word for word ───────────────────────────
vm_args=(exec -- bash -c 'echo "a b"' x)
run env >/dev/null 2>&1 || bad "exec exits 0" "$?"
got="$(cat "$calls")"
[ "$got" = 'exec|--|bash|-c|echo "a b"|x' ] || bad "exec keeps every word of the command" "$got"

# ── 5. exec with nothing to run ────────────────────────────────────────────
vm_args=(exec --)
if err="$(run env 2>&1)"; then bad "an empty exec fails" "exit 0"; fi
case "$err" in *"exec needs a command"*) ;; *) bad "an empty exec says what is missing" "$err" ;; esac

# ── 6. No harness beside the repo ──────────────────────────────────────────
mv "$tmp/fs-windows-test-harness" "$tmp/elsewhere"
vm_args=(status)
if err="$(run env 2>&1)"; then bad "a missing harness fails" "exit 0"; fi
case "$err" in *"chore siblings"*) ;; *) bad "a missing harness names the task that provides it" "$err" ;; esac

[ "$fails" -eq 0 ] || exit 1
echo "vm.sh: 6 checks passed"
