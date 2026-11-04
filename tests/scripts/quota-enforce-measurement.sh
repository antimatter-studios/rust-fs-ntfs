#!/usr/bin/env bash
# What Windows does with a write past an enforced quota limit is the whole
# output of win-format-win-quota-enforce-win-chkdsk (#867), and that output sits
# on the runner: quota-enforce.txt in the VM's diag directory, and the step's
# stdout under test-diagnostics/matrix/, which the next run-matrix.sh replaces.
# It comes home like every scenario's (tests/scripts/windows-job-diagnostics.sh,
# #1245), and has no fetch or upload of its own.
#
# This holds CI to failing when the report is not there, and to keeping it past
# the job in the job's artifact.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fails=0
fail() { fails=$((fails + 1)); printf 'FAIL %s\n' "$*"; }
workflow="$ROOT/.github/workflows/ci.yml"
op="$ROOT/scripts/fs-windows-test-harness/win-quota-enforce.ps1"
scenario='win-format-win-quota-enforce-win-chkdsk'

# From the step that checks the report up to the next step.
section="$(awk '
    /- name: Check the quota limit measurement came home/ { on = 1; print; next }
    on && /^      - / { exit }
    on
' "$workflow")"
[[ -n "$section" ]] || fail 'CI has no step checking that the quota limit measurement came home'

grep -Fq 'id: quota_enforce' "$workflow" || fail 'the measuring step has no id for the step after it to test'
grep -Fq "run-matrix.sh $scenario" "$workflow" || fail 'CI has no step measuring a write past an enforced quota limit'
grep -Fq 'quota-enforce.txt' <<<"$section" || fail 'nothing fails when quota-enforce.txt did not come home'
grep -Fq 'exit 1' <<<"$section" || fail 'a missing quota-enforce.txt does not fail the step'
grep -Fq "steps.quota_enforce.outcome != 'skipped'" <<<"$section" \
    || fail 'the check is not tied to the measuring step having run'
grep -Fq 'bash scripts/matrix-fetch-diag.sh' <<<"$section" && fail 'the measurement has a fetch of its own beside the job'"'"'s'
grep -Fq 'actions/upload-artifact' <<<"$section" && fail 'the measurement has an upload of its own beside the job'"'"'s'

# The job's own artifact carries it, for as long as its own upload did.
upload="$(awk '
    /- name: Upload diagnostics/ { on = 1; print; next }
    on && /^      - / { exit }
    on
' "$workflow")"
grep -Fq 'test-diagnostics/matrix-groups/' <<<"$upload" || fail 'the upload omits the kept scenario diagnostics'
grep -Fq 'test-diagnostics/matrix/' <<<"$upload" || fail 'the upload omits the last group'
grep -Fq 'retention-days:' <<<"$upload" || fail 'the upload has no retention bound'
grep -Eq '^[[:space:]]*[^![:space:]].*\.img' <<<"$upload" && fail 'the upload retains a disk image'

# In PowerShell -f binds tighter than +, so it formats only the string beside
# it: a placeholder in an earlier concatenated string prints as written.
awk '
    /\{[0-9]+(:[^}]*)?\}/ && /"[[:space:]]*\+[[:space:]]*$/ { bad = bad "\n  " FILENAME ":" FNR ": " $0 }
    END { if (bad != "") { print "FAIL a placeholder is left out of its -f:" bad; exit 1 } }
' "$op" || fails=$((fails + 1))

if (( fails == 0 )); then printf 'quota-enforce-measurement.sh: all checks passed\n'; else exit 1; fi
