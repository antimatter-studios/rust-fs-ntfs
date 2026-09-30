#!/usr/bin/env bash
# The script-test tier fails when ANY script test fails, not only the last.
#
# A `for t in ...; do bash "$t"; done` loop exits with the status of its last
# command, so an earlier failing test reached neither `chore test:scripts` nor
# CI (#362). This takes the tier command out of both places that run it --
# the `test:scripts` task in chores.yml and the `shell tests` step in ci.yml --
# points it at a scratch directory, and requires:
#
#   * a failing test that is NOT last fails the tier, and is named with the
#     status it exited with;
#   * the tests after it still run, so one run reports every failure;
#   * a directory of passing tests passes;
#   * a directory with no tests fails, because nothing ran.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
pass=0
fail=0

ok()  { pass=$((pass + 1)); }
bad() { fail=$((fail + 1)); printf 'FAIL %s\n' "$*"; }

sandbox="$(mktemp -d)"
trap 'rm -rf "$sandbox"' EXIT

# The command after `tier.sh scripts --` in chores.yml's test:scripts task.
# tier.sh itself is not run: it would overwrite tmp/logs/scripts.log, the log
# of the very tier this test is running inside.
chores_cmd="$(awk '
    $0 == "  test:scripts:" { task = 1; next }
    task && /^  [a-z][a-z:_-]*:$/ { exit }
    task && /^      - scripts\/tier\.sh scripts -- / {
        sub(/^      - scripts\/tier\.sh scripts -- /, ""); print; exit
    }
' "$ROOT/chores.yml")"

# The `run:` of the step named `shell tests` in ci.yml.
ci_cmd="$(awk '
    /^      - name: shell tests$/ { step = 1; next }
    step && /^      - / { exit }
    step && /^        run: / { sub(/^        run: /, ""); print; exit }
' "$ROOT/.github/workflows/ci.yml")"

[ -n "$chores_cmd" ] && ok || bad "chores.yml test:scripts runs through tier.sh scripts"
[ -n "$ci_cmd" ] && ok || bad "ci.yml has a single-line run: for the shell tests step"

# Scratch test directories.
mixed="$sandbox/mixed"; passing="$sandbox/passing"; empty="$sandbox/empty"
mkdir -p "$mixed" "$passing" "$empty"
# 3, not 1: a status nothing else in the run produces, so the report can
# only have taken it from the test.
printf '#!/usr/bin/env bash\nexit 3\n' > "$mixed/a-fails.sh"
printf '#!/usr/bin/env bash\ntouch "%s/ran-b"\nexit 0\n' "$sandbox" > "$mixed/b-passes.sh"
printf '#!/usr/bin/env bash\nexit 0\n' > "$passing/a-passes.sh"
printf '#!/usr/bin/env bash\nexit 0\n' > "$passing/b-passes.sh"

# Run a tier command against DIR, from the repository root, as CI does.
run_tier() {
    local cmd="$1" dir="$2"
    cmd="${cmd//tests\/scripts/$dir}"
    (cd "$ROOT" && bash -c "$cmd") > "$sandbox/out" 2>&1
}

check_cmd() {
    local where="$1" cmd="$2"
    [ -n "$cmd" ] || return
    case "$cmd" in
        *tests/scripts*) ok ;;
        *) bad "$where: the tier command names tests/scripts"; return ;;
    esac

    rm -f "$sandbox/ran-b"
    if run_tier "$cmd" "$mixed"; then
        bad "$where: a failing test that is not last fails the tier"
    else
        ok
    fi
    grep -q 'a-fails\.sh' "$sandbox/out" && ok \
        || bad "$where: the failing test is named in the tier's output"
    grep -q 'a-fails\.sh (exit 3)' "$sandbox/out" && ok \
        || bad "$where: the failing test's own exit status is named, not $(grep 'a-fails' "$sandbox/out")"
    [ -f "$sandbox/ran-b" ] && ok \
        || bad "$where: the tests after a failure still run"

    run_tier "$cmd" "$passing" && ok \
        || bad "$where: a directory of passing tests passes"

    if run_tier "$cmd" "$empty"; then
        bad "$where: a directory with no tests fails -- nothing ran"
    else
        ok
    fi
}

check_cmd "chores.yml test:scripts" "$chores_cmd"
check_cmd "ci.yml shell tests" "$ci_cmd"

printf 'script-tier-status: %d passed, %d failed\n' "$pass" "$fail"
exit $(( fail > 0 ))
