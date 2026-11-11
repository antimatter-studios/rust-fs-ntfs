#!/usr/bin/env bash
# The run's depth by event, from scripts/ci-plan.sh: every pull request runs
# the full pipeline, including stacked layers. Only pushes to main are quick,
# because the merge queue already validated that tree.
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

plan pull-request pull_request refs/pull/1/merge true
plan stacked-pull-request pull_request refs/pull/2/merge true
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
if EVENT=pull_request REF=refs/pull/1/merge GITHUB_OUTPUT='' bash "$ROOT/scripts/ci-plan.sh" >/dev/null 2>&1; then
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
for workflow in "$ROOT"/.github/workflows/*.yml; do
    if ! grep -q '^  pull_request:' "$workflow"; then
        continue
    fi
    if sed -n '/^  pull_request:/,/^  [a-z_]*:/p' "$workflow" |
        grep -Eq '^    (paths|paths-ignore|branches|branches-ignore):'; then
        bad "$(basename "$workflow"): every pull request must run without path or branch filters"
    else
        ok
    fi
done
grep -Fq '      mkfs: ${{ steps.code.outputs.code }}' "$CI" && ok || bad "Windows validation does not follow the full-pipeline plan"
# ci-ok still reads the planner's decision.
grep -q 'CODE: \${{ needs.changes.outputs.code }}' "$CI" && ok || bad "ci-ok does not read the run's depth"
for job in validate-mkfs-windows ci-ok; do
    verdict="$(sed -n "/^  $job:/,/^  [a-z_]*:/p" "$CI")"
    printf '%s\n' "$verdict" | grep -Fq 'EVENT: ${{ github.event_name }}' && ok || bad "$job does not bind skipped-job exceptions to the event"
    printf '%s\n' "$verdict" | grep -Fq 'REF: ${{ github.ref }}' && ok || bad "$job does not bind skipped-job exceptions to main"
done

box_cache="$(sed -n '/^  box-cache:/,/^  kernel:/p' "$CI")"
printf '%s\n' "$box_cache" | grep -q "^    if: github.event_name == 'push' && github.ref == 'refs/heads/main'$" && ok || bad "box cache must run only on a push to main"
printf '%s\n' "$box_cache" | grep -q '^          chore siblings$' && ok || bad "box cache does not use the pinned siblings"
printf '%s\n' "$box_cache" | grep -q 'ci-setup-linux.sh --box-cache-key' && ok || bad "box cache key does not come from the pinned harness"
printf '%s\n' "$box_cache" | grep -q 'key: \${{ steps.vm-host.outputs.box-cache-key }}' && ok || bad "box cache key differs from the kernel job"
printf '%s\n' "$box_cache" | grep -q 'path: ~/.vagrant.d/boxes' && ok || bad "box cache path differs from the kernel job"
printf '%s\n' "$box_cache" | grep -q 'vagrant box add "$box" --box-version "$box_version" --provider libvirt --architecture amd64' && ok || bad "box cache does not fetch the pinned Linux box"
if printf '%s\n' "$box_cache" | grep -Eq 'needs:|vagrant up|chore (test|linux:up|vm:up)'; then
    bad "box cache must not depend on a suite or boot a VM"
else
    ok
fi
grep -q "CI_GATE_NON_GATING='asan box-cache validate-mkfs-windows'" "$ROOT/chores.yml" && ok || bad "main-only box cache is not declared non-gating"
if sed -n '/^  ci-ok:/,$p' "$CI" | grep -q 'needs:.*box-cache'; then
    bad "box cache must not lengthen the merge queue path"
else
    ok
fi

printf 'ci plan: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
