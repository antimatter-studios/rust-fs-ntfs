#!/usr/bin/env bash
# run-script-tests.sh [DIR]
#
# Run every DIR/*.sh (default tests/scripts) and fail if ANY of them failed.
#
# This replaced `for t in tests/scripts/*.sh; do bash "$t"; done`, whose exit
# status was the LAST test's: any earlier test could fail while the tier and
# CI stayed green (#362). Every test runs even after one fails, so a single run
# reports all of them, and the failures are named at the end.
#
# The count is of tests that RAN, not files found. A directory with no tests
# fails: a tier that ran nothing is not a tier that passed.
#
# tests/scripts/script-tier-status.sh holds chores.yml and ci.yml to this.
set -uo pipefail

dir="${1:-tests/scripts}"
ran=0
failed=()

shopt -s nullglob
for t in "$dir"/*.sh; do
    ran=$((ran + 1))
    bash "$t" || failed+=("$(basename "$t") (exit $?)")
done

if [ "$ran" -eq 0 ]; then
    echo "run-script-tests: no tests ran from $dir/*.sh" >&2
    exit 1
fi

if [ "${#failed[@]}" -gt 0 ]; then
    echo "run-script-tests: ${#failed[@]} of $ran script test(s) failed:" >&2
    printf '    %s\n' "${failed[@]}" >&2
    exit 1
fi

echo "run-script-tests: $ran script test(s) ran, all passed"
