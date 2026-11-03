#!/usr/bin/env bash
# What Windows does with a write past an enforced quota limit is the whole
# output of win-format-win-quota-enforce-win-chkdsk (#867), and that output sits
# on the runner: quota-enforce.txt in the VM's diag directory, and the step's
# stdout under test-diagnostics/matrix/, which the next run-matrix.sh replaces.
# The job's own fetch is `if: failure()`, so a green run used to lose it.
#
# This holds CI to bringing the report home before the next scenario runs, to
# failing when the report is not there, and to keeping it past the job.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fails=0
fail() { fails=$((fails + 1)); printf 'FAIL %s\n' "$*"; }
workflow="$ROOT/.github/workflows/ci.yml"
op="$ROOT/scripts/fs-windows-test-harness/win-quota-enforce.ps1"
scenario='win-format-win-quota-enforce-win-chkdsk'

# From the measuring step up to the next step that runs another scenario.
section="$(awk -v scenario="$scenario" '
    /- name: Measure a write past an enforced quota limit on Windows/ { on = 1 }
    on && /run-matrix\.sh/ && index($0, scenario) == 0 { exit }
    on
' "$workflow")"
[[ -n "$section" ]] || fail 'CI has no step measuring a write past an enforced quota limit'

grep -Fq 'id: quota_enforce' <<<"$section" || fail 'the measuring step has no id for the steps after it to test'
grep -Fq 'bash scripts/matrix-fetch-diag.sh' <<<"$section" \
    || fail 'the VM diag directory is not fetched before the next scenario runs'
grep -Fq 'quota-enforce.txt' <<<"$section" || fail 'nothing fails when quota-enforce.txt did not come home'
grep -Fq 'actions/upload-artifact' <<<"$section" || fail 'the measurement is not uploaded'
grep -Fq "test-diagnostics/matrix/$scenario" <<<"$section" || fail 'the upload omits the scenario diagnostics'
grep -Fq 'retention-days:' <<<"$section" || fail 'the upload has no retention bound'
[[ "$(grep -Fc "steps.quota_enforce.outcome != 'skipped'" <<<"$section")" -ge 2 ]] \
    || fail 'fetching and uploading are not both tied to the measuring step having run'
grep -Fq '.img' <<<"$section" && fail 'the upload retains a disk image'

# In PowerShell -f binds tighter than +, so it formats only the string beside
# it: a placeholder in an earlier concatenated string prints as written.
awk '
    /\{[0-9]+(:[^}]*)?\}/ && /"[[:space:]]*\+[[:space:]]*$/ { bad = bad "\n  " FILENAME ":" FNR ": " $0 }
    END { if (bad != "") { print "FAIL a placeholder is left out of its -f:" bad; exit 1 } }
' "$op" || fails=$((fails + 1))

if (( fails == 0 )); then printf 'quota-enforce-measurement.sh: all checks passed\n'; else exit 1; fi
