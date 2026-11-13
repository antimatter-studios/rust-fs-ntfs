#!/usr/bin/env bash
# Every job that runs on a Windows runner says how long it may take (#388).
#
# Without `timeout-minutes` a job inherits GitHub's six hours. The Windows
# jobs drive the runner through SSH to itself (scripts/run-matrix.sh
# --vm-host=runneradmin@localhost), and a session that stops answering
# prints nothing: on 2026-09-30 one sat 66 minutes after its last line in a
# step that takes 4, holding the PR BLOCKED with nothing in the log to say
# why. A bound turns that into a failure naming the job.
#
# Checked in every workflow, not only ci.yml: release.yml's Windows jobs
# gate a publish, and a hang there holds a release. The job has to declare
# its own bound; a job-level key is what GitHub applies to every step.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
pass=0
fail=0

ok()  { pass=$((pass + 1)); }
bad() { fail=$((fail + 1)); printf 'FAIL %s\n' "$*"; }

# Prints `<job> <windows 0|1> <timeout or ->` for each job in a workflow.
jobs_of() {
    awk '
        function flush() {
            if (job != "") print job, win, (limit == "" ? "-" : limit)
        }
        /^jobs:$/ { in_jobs = 1; next }
        in_jobs && /^[^[:space:]#]/ { flush(); job = ""; in_jobs = 0 }
        in_jobs && /^  [A-Za-z0-9_-]+:[[:space:]]*$/ {
            flush()
            job = $1; sub(/:$/, "", job); win = 0; limit = ""
            next
        }
        in_jobs && job != "" && /^    runs-on:.*windows/ { win = 1 }
        in_jobs && job != "" && /^    timeout-minutes:[[:space:]]*[0-9]+[[:space:]]*$/ {
            limit = $2
        }
        END { flush() }
    ' "$1"
}

# The check, over one workflow; prints a line per unbounded Windows job.
unbounded_windows_jobs() {
    jobs_of "$1" | awk '$2 == 1 && $3 == "-" { print $1 }'
}

oracle_budget_covers_work() {
    jobs_of "$1" | awk -v minimum_minutes=120 '
        $1 == "validate-mkfs-windows-run" && $2 == 1 && $3 >= minimum_minutes {
            sufficient = 1
        }
        END { exit !sufficient }
    '
}

native_fixture_budget_covers_work() {
    jobs_of "$1" | awk -v minimum_minutes=45 '
        $1 == "windows-native-read-fixtures" && $2 == 1 && $3 >= minimum_minutes {
            sufficient = 1
        }
        END { exit !sufficient }
    '
}

# A control: the parser must see Windows jobs at all, or the check below
# passes by finding nothing. ci.yml has two and release.yml two.
for wf in ci.yml release.yml; do
    n="$(jobs_of "$ROOT/.github/workflows/$wf" | awk '$2 == 1' | wc -l)"
    if [ "$n" -ge 2 ]; then ok; else bad "$wf: found $n Windows job(s), expected at least 2 -- the parser no longer reads this workflow"; fi
done

shopt -s nullglob
workflows=("$ROOT"/.github/workflows/*.yml)
[ "${#workflows[@]}" -gt 0 ] || bad "no workflows under .github/workflows"
for wf in "${workflows[@]}"; do
    missing="$(unbounded_windows_jobs "$wf")"
    if [ -z "$missing" ]; then
        ok
    else
        bad "$(basename "$wf"): Windows job(s) with no timeout-minutes: $(echo $missing)"
    fi
done

if oracle_budget_covers_work "$ROOT/.github/workflows/ci.yml"; then
    ok
else
    bad "ci.yml: Windows oracle needs at least 120 minutes: 70 before CLI, 30 for CLI, 20 for trailing checks and runner variance"
fi

if native_fixture_budget_covers_work "$ROOT/.github/workflows/ci.yml"; then
    ok
else
    bad "ci.yml: windows-native-read-fixtures needs at least 45 minutes: run 43228476645 hit 30 at step 28 of 34"
fi

# The check must fail on the case it exists for: a Windows job with no bound.
sandbox="$(mktemp -d)"
trap 'rm -rf "$sandbox"' EXIT
cat > "$sandbox/unbounded.yml" <<'EOF'
jobs:
  linux:
    runs-on: ubuntu-latest
    steps: []
  hung:
    name: a Windows job
    runs-on: windows-latest
    steps:
      - run: |
          timeout-minutes: 5
EOF
if [ "$(unbounded_windows_jobs "$sandbox/unbounded.yml")" = hung ]; then ok; else bad "an unbounded Windows job was not caught (a step's text must not count as the job's bound)"; fi
sed 's/^    name: a Windows job$/    timeout-minutes: 30/' "$sandbox/unbounded.yml" > "$sandbox/bounded.yml"
if [ -z "$(unbounded_windows_jobs "$sandbox/bounded.yml")" ]; then ok; else bad "a bounded Windows job was reported as unbounded"; fi

for timeout_minutes in 85 119 120 180; do
    fixture="$sandbox/oracle-$timeout_minutes.yml"
    printf 'jobs:\n  validate-mkfs-windows-run:\n    runs-on: windows-latest\n    timeout-minutes: %s\n' "$timeout_minutes" > "$fixture"
    if oracle_budget_covers_work "$fixture"; then
        if [ "$timeout_minutes" -ge 120 ]; then ok; else bad "the insufficient $timeout_minutes-minute oracle budget was accepted"; fi
    else
        if [ "$timeout_minutes" -lt 120 ]; then ok; else bad "the sufficient $timeout_minutes-minute oracle budget was rejected"; fi
    fi
done

for timeout_minutes in 30 44 45 60; do
    fixture="$sandbox/native-$timeout_minutes.yml"
    printf 'jobs:\n  windows-native-read-fixtures:\n    runs-on: windows-latest\n    timeout-minutes: %s\n' "$timeout_minutes" > "$fixture"
    if native_fixture_budget_covers_work "$fixture"; then
        if [ "$timeout_minutes" -ge 45 ]; then ok; else bad "the insufficient $timeout_minutes-minute native fixture budget was accepted"; fi
    else
        if [ "$timeout_minutes" -lt 45 ]; then ok; else bad "the sufficient $timeout_minutes-minute native fixture budget was rejected"; fi
    fi
done

if [ "$fail" -gt 0 ]; then
    echo "windows-job-timeouts: $pass passed, $fail failed"
    exit 1
fi
echo "windows-job-timeouts: $pass checks passed"
