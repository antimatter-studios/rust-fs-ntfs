#!/usr/bin/env bash
# tier-skip-gate.sh runs the real tier.sh on synthetic output. It must not
# write the checkout's own tmp/logs/unit.log, finished or still being
# written (#1353, #1403). It runs here in a throwaway repository, so no real
# log is at risk whichever way this test goes.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
core="$(cd "$ROOT/../rust-fs-core" && pwd)" || {
    echo "FAIL: ../rust-fs-core is missing; run chore siblings" >&2
    exit 1
}
sandbox="$(mktemp -d)"
trap 'rm -rf "$sandbox"' EXIT
fails=0

repo="$sandbox/repo"
git init --quiet "$repo" || exit 1
mkdir -p "$repo/tests/scripts" "$repo/tmp/logs" || exit 1
cp "$ROOT/tests/scripts/tier-skip-gate.sh" "$repo/tests/scripts/" || exit 1
ln -s "$core" "$sandbox/rust-fs-core" || exit 1
log="$repo/tmp/logs/unit.log"

run_gate() {
    local when="$1"
    if ! env -u FS_CORE_CALLER bash "$repo/tests/scripts/tier-skip-gate.sh" \
        >"$sandbox/gate.out" 2>&1 || ! grep -q '^5 passed, 0 failed$' "$sandbox/gate.out"; then
        echo "FAIL: $when: the five skip-gate cases did not all pass" >&2
        cat "$sandbox/gate.out" >&2
        fails=$((fails + 1))
    fi
}

check_log() {
    if ! cmp -s "$sandbox/expected.log" "$log"; then
        echo "FAIL: $1: the existing unit.log changed" >&2
        fails=$((fails + 1))
    fi
}

# Sequential: a finished unit log is already there.
printf 'finished unit run\ntest result: ok. 834 passed\n' >"$log"
cp "$log" "$sandbox/expected.log"
run_gate sequential
check_log sequential

# Concurrent: a unit run holds the log open and writes on both sides.
exec 3>"$log"
printf 'active unit run, first half\n' >&3
run_gate concurrent
printf 'active unit run, second half\n' >&3
exec 3>&-
printf 'active unit run, first half\nactive unit run, second half\n' >"$sandbox/expected.log"
check_log concurrent

[ "$fails" -eq 0 ] || exit 1
echo "tier-skip-gate-isolation.sh: all checks passed"
