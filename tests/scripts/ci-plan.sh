#!/usr/bin/env bash
# The run's depth by event, from scripts/ci-plan.sh: pull requests and pushes
# to main are quick; the merge queue, manual dispatches, tags and anything
# unexpected run the full pipeline. The workflow must call that script, and
# the Windows filter must run only for the merge queue.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CI="$ROOT/.github/workflows/ci.yml"
pass=0
fail=0

ok() { pass=$((pass + 1)); }
bad() { fail=$((fail + 1)); printf 'FAIL %s\n' "$1"; }

plan() {
    local name="$1" event="$2" ref="$3" want="$4" out got
    out="$(mktemp)"
    if EVENT="$event" REF="$ref" GITHUB_OUTPUT="$out" bash "$ROOT/scripts/ci-plan.sh" >/dev/null 2>&1; then
        got="$(cat "$out")"
    else
        got="exit $?"
    fi
    rm -f "$out"
    if [ "$got" = "code=$want" ]; then ok; else bad "$name: got '$got', expected code=$want"; fi
}

plan pull-request pull_request refs/pull/1/merge false
plan stacked-pull-request pull_request refs/pull/2/merge false
plan merge-queue merge_group refs/heads/gh-readonly-queue/main/pr-1-abc true
plan dispatch-branch workflow_dispatch refs/heads/ci/oracle-red true
plan dispatch-main workflow_dispatch refs/heads/main true
plan push-main push refs/heads/main false
plan push-tag push refs/tags/v1.0.0 true
plan push-other-branch push refs/heads/feature true
plan workflow-call workflow_call refs/tags/v1.0.0 true
plan unexpected-event schedule refs/heads/main true
plan missing-event '' '' true

# No GITHUB_OUTPUT: the planner must fail rather than decide nothing.
if EVENT=pull_request REF=refs/pull/1/merge GITHUB_OUTPUT= bash "$ROOT/scripts/ci-plan.sh" >/dev/null 2>&1; then
    bad "missing GITHUB_OUTPUT accepted"
else
    ok
fi

# The workflow runs this planner, on every pull request base, and gives the
# merge queue and manual dispatches a run.
grep -q '^        run: bash scripts/ci-plan.sh$' "$CI" && ok || bad "ci.yml does not run scripts/ci-plan.sh"
grep -q '^  merge_group:' "$CI" && ok || bad "ci.yml has no merge_group trigger"
grep -q '^  workflow_dispatch:' "$CI" && ok || bad "ci.yml has no workflow_dispatch trigger"
if sed -n '/^  pull_request:/,/^  [a-z_]*:/p' "$CI" | grep -q 'branches:'; then
    bad "pull_request is limited to some branches; stacked layers get no run"
else
    ok
fi
grep -q "^        if: github.event_name == 'merge_group'$" "$CI" && ok || bad "the Windows filter is not merge-queue only"
grep -q "^        if: github.event_name != 'merge_group'$" "$CI" && ok || bad "the unfiltered Windows decision does not cover every other event"
# ci-ok still reads the planner's decision.
grep -q 'CODE: \${{ needs.changes.outputs.code }}' "$CI" && ok || bad "ci-ok does not read the run's depth"

printf 'ci plan: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
