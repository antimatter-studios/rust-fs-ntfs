#!/usr/bin/env bash
# Exercise the exact verdict command that CI runs, including missing/skipped
# dependencies. A green aggregate must never launder an absent gate.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
pass=0
fail=0

check() {
    local name="$1" mode="$2" want="$3" json="$4" code="${5:-}" got
    CODE="$code" GATE_NEEDS_JSON="$json" bash "$ROOT/scripts/ci-verdict.sh" "$mode" >/dev/null 2>&1
    got=$?
    if { [ "$want" = pass ] && [ "$got" -eq 0 ]; } ||
       { [ "$want" = fail ] && [ "$got" -ne 0 ]; }; then
        pass=$((pass + 1))
    else
        fail=$((fail + 1))
        printf 'FAIL %s: exit %s, expected %s\n' "$name" "$got" "$want"
    fi
}

windows_true='{"changes":{"result":"success","outputs":{"mkfs":"true"}},"validate-mkfs-windows-run":{"result":"success"}}'
windows_false='{"changes":{"result":"success","outputs":{"mkfs":"false"}},"validate-mkfs-windows-run":{"result":"skipped"}}'
aggregate='{"test":{"result":"success"},"windows-native-read-fixtures":{"result":"success"},"integration":{"result":"success"},"changes":{"result":"success"},"validate-mkfs-windows":{"result":"success"},"cli":{"result":"success"},"kernel":{"result":"success"},"semver":{"result":"success"}}'

check windows-required-ran windows pass "$windows_true"
check windows-unneeded-skipped windows pass "$windows_false"
check windows-required-skipped windows fail "$(jq -c '.["validate-mkfs-windows-run"].result="skipped"' <<< "$windows_true")"
check windows-unneeded-failed windows fail "$(jq -c '.["validate-mkfs-windows-run"].result="failure"' <<< "$windows_false")"
check windows-filter-failed windows fail "$(jq -c '.changes.result="failure"' <<< "$windows_true")"
check windows-missing-run windows fail '{"changes":{"result":"success","outputs":{"mkfs":"true"}}}'
check windows-missing-decision windows fail '{"changes":{"result":"success"},"validate-mkfs-windows-run":{"result":"skipped"}}'

check aggregate-green aggregate pass "$aggregate" true
# semver and windows-native-read-fixtures are in ci-ok's `needs:` and were
# once absent from the verdict's list, so a failing semver job left ci-ok
# green (#409).
for job in test windows-native-read-fixtures integration changes validate-mkfs-windows cli kernel semver; do
    for result in failure cancelled skipped; do
        check "aggregate-$job-$result" aggregate fail \
            "$(jq -c --arg job "$job" --arg result "$result" '.[$job].result=$result' <<< "$aggregate")" true
    done
done
# A change to documentation alone skips the Windows fixtures and what reads
# them, and the kernel oracle. Their skip is green only when the changes job
# said code is false.
docs_only="$(jq -c '.["windows-native-read-fixtures"].result="skipped" | .integration.result="skipped" | .cli.result="skipped" | .kernel.result="skipped"' <<< "$aggregate")"
check aggregate-docs-only-skips aggregate pass "$docs_only" false
check aggregate-code-skips aggregate fail "$docs_only" true
check aggregate-unknown-code-skips aggregate fail "$docs_only"
check aggregate-docs-only-failure aggregate fail "$(jq -c '.cli.result="failure"' <<< "$docs_only")" false
check aggregate-docs-only-changes-skipped aggregate fail "$(jq -c '.changes.result="skipped"' <<< "$docs_only")" false
check aggregate-missing-integration aggregate fail '{"test":{"result":"success"},"changes":{"result":"success"},"validate-mkfs-windows":{"result":"success"},"cli":{"result":"success"}}' true
check aggregate-missing-cli aggregate fail '{"test":{"result":"success"},"integration":{"result":"success"},"changes":{"result":"success"},"validate-mkfs-windows":{"result":"success"}}' true
check aggregate-missing-semver aggregate fail "$(jq -c 'del(.semver)' <<< "$aggregate")" true
check aggregate-missing-kernel aggregate fail "$(jq -c 'del(.kernel)' <<< "$aggregate")" true
# A job added to ci-ok's `needs:` is judged even before this list learns it.
check aggregate-extra-need-failed aggregate fail "$(jq -c '.["new-job"].result="failure"' <<< "$aggregate")" true
check aggregate-invalid-json aggregate fail '{' true
# The run's depth comes from scripts/ci-plan.sh and is true or false. A
# missing or invalid decision is red even when every job succeeded.
check aggregate-missing-plan aggregate fail "$aggregate"
check aggregate-invalid-plan aggregate fail "$aggregate" maybe
check aggregate-quick-green aggregate pass "$aggregate" false
# A quick run (a pull request or a push to main) excuses only the full
# jobs' skip. A quick job skipped, and any job failed or cancelled, is red.
for job in test changes validate-mkfs-windows semver; do
    check "aggregate-quick-$job-skipped" aggregate fail \
        "$(jq -c --arg job "$job" '.[$job].result="skipped"' <<< "$docs_only")" false
done
for job in test windows-native-read-fixtures integration changes validate-mkfs-windows cli kernel semver; do
    for result in failure cancelled; do
        check "aggregate-quick-$job-$result" aggregate fail \
            "$(jq -c --arg job "$job" --arg result "$result" '.[$job].result=$result' <<< "$docs_only")" false
    done
done
check aggregate-quick-extra-need-skipped aggregate fail "$(jq -c '.["new-job"].result="skipped"' <<< "$docs_only")" false
check aggregate-quick-missing-kernel aggregate fail "$(jq -c 'del(.kernel)' <<< "$docs_only")" false
check aggregate-full-run-skips aggregate fail "$docs_only" true
# python-volume-info-windows reads the Windows-authored fixtures, so a quick
# run skips it with them, as ci-ok saw on every pull request once it joined
# `needs:`. python-abi is a quick job and is never excused a skip.
python_quick="$(jq -c '.["python-abi"].result="success" | .["python-volume-info-windows"].result="skipped"' <<< "$docs_only")"
check aggregate-quick-python-windows-skipped aggregate pass "$python_quick" false
check aggregate-full-python-windows-skipped aggregate fail "$(jq -c '.["python-volume-info-windows"].result="skipped"' <<< "$aggregate")" true
check aggregate-quick-python-windows-failure aggregate fail "$(jq -c '.["python-volume-info-windows"].result="failure"' <<< "$python_quick")" false
check aggregate-quick-python-abi-skipped aggregate fail "$(jq -c '.["python-abi"].result="skipped"' <<< "$python_quick")" false

printf 'ci verdict: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
