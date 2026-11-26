#!/usr/bin/env bash
# Tests for the Windows fixture producers' shared helper (issue #629).
#
# capture-filename-fields.ps1 once called New-WindowsFixture without loading
# it, and the hosted Windows run failed with "not recognized" (run
# 38837437553). That is a text-level contract a Linux runner can hold before a
# Windows runner is spent: the helper is defined once, in a file with no
# top-level side effects, and every producer that calls it loads that file.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT" || exit 1

pass=0; fail=0
ok() { pass=$((pass + 1)); }
bad() { fail=$((fail + 1)); printf '  FAIL  %s\n' "$1"; }

helper=test-disks/windows-fixture.ps1
producers=(test-disks/build-windows-native-read-fixtures.ps1 test-disks/capture-filename-fields.ps1)
workflow=.github/workflows/security-descriptor-oracle.yml

printf 'windows-fixture-producers\n'

[ "$(grep -c '^function New-WindowsFixture' "$helper")" -eq 1 ] \
    && ok || bad "$helper defines New-WindowsFixture exactly once"

# Outside the function body the helper holds only comments and blank lines,
# so dot-sourcing it cannot build, mount or delete anything.
top_level=$(awk '/^function /{inside=1} inside && /^}/{inside=0; next} !inside && !/^[[:space:]]*(#|$)/' "$helper")
[ -z "$top_level" ] && ok || bad "$helper has top-level statements: $top_level"

for p in "${producers[@]}"; do
    grep -q '^\. "\$PSScriptRoot\\windows-fixture\.ps1"' "$p" \
        && ok || bad "$p dot-sources windows-fixture.ps1"
    if grep -q '^function New-WindowsFixture' "$p"; then bad "$p redefines New-WindowsFixture"; else ok; fi
done

grep -q 'Assert-VolumeClean -Root \$root -Step \$step' test-disks/capture-filename-fields.ps1 \
    && ok || bad "every capture stage runs chkdsk while the volume is mounted"
grep -q 'chkdsk needs a drive letter' test-disks/capture-filename-fields.ps1 \
    && ok || bad "a stage without a drive letter fails by name"
grep -q 'has no chkdsk log' "$workflow" \
    && ok || bad "$workflow requires a chkdsk log per step"

# Cleanup runs after the upload, on failure too, and names only the six
# generated stage images.
upload_line=$(grep -n 'name: filename-fields-' "$workflow" | head -1 | cut -d: -f1)
cleanup_line=$(grep -n 'generated stage images are removed after the artifact handoff' "$workflow" | cut -d: -f1)
if [ -n "$upload_line" ] && [ -n "$cleanup_line" ] && [ "$cleanup_line" -gt "$upload_line" ]; then ok
else bad "$workflow removes the stage images after the artifact upload"; fi
cleanup=$(sed -n "${cleanup_line:-1},\$p" "$workflow" | sed -n '1,10p')
grep -q 'if: always()' <<<"$cleanup" && ok || bad "the cleanup step runs on failure too"
grep -q 'Remove-Item -LiteralPath "test-disks/ntfs-fn-fields-step\$step.img"' <<<"$cleanup" \
    && ok || bad "the cleanup removes only the named stage images"
if grep -qE 'Remove-Item[^|]*\*' <<<"$cleanup"; then bad "the cleanup uses a wildcard"; else ok; fi

# Every named producer runs under the out-of-process watchdog (#1395): the
# in-process phase timer cannot stop a blocked .NET or CIM storage call. The
# same job first proves on its own Windows runner that a blocked phase times
# out, and keeps the watchdog's diagnostics when the producer fails.
if problems=$(python3 - <<'PY'
import pathlib
import re

import yaml

CONTROL = 'FixtureProcess.test_blocked_dotnet_phase_and_its_descendant_end_at_the_phase_deadline'
EXPECTED = {
    'ci.yml': {'build-windows-native-read-fixtures.ps1': 1},
    'release.yml': {'build-windows-native-read-fixtures.ps1': 1},
    'security-descriptor-oracle.yml': {'capture-filename-fields.ps1': 1},
    'slate-oracle.yml': {'build-slate-oracle-cases.ps1': 2},
}
problems = []
for workflow, producers in EXPECTED.items():
    jobs = yaml.safe_load(pathlib.Path('.github/workflows', workflow).read_text())['jobs']
    for producer, count in producers.items():
        found = 0
        for name, job in jobs.items():
            steps = job.get('steps', [])
            for index, step in enumerate(steps):
                run = ' '.join(str(step.get('run', '')).split())
                if not re.search(r'(-File |\./)test-disks/' + re.escape(producer), run):
                    continue
                found += 1
                where = f'{workflow} {name}: {producer}'
                wrapped = re.match(
                    r'python scripts/run-fixture-process\.py --watch-phases --timeout-seconds (\d+) '
                    r'--log-dir (tmp/\S+) --lifecycle \2/lifecycle\.log -- pwsh -NoProfile -File ', run)
                if not wrapped:
                    problems.append(f'{where} runs without the process watchdog')
                    continue
                limit, logs = int(wrapped.group(1)) / 60, wrapped.group(2)
                budget = job.get('timeout-minutes', 360)
                if limit >= budget:
                    problems.append(f'{where} watchdog ({limit:g} min) cannot fire inside the {budget} min job')
                if not any(CONTROL in str(s.get('run', '')) for s in steps[:index]):
                    problems.append(f'{where} has no blocked-phase control before it')
                if not any('always()' in str(s.get('if', ''))
                           and str(s.get('uses', '')).startswith('actions/upload-artifact@')
                           and logs + '/' in str(s.get('with', {}).get('path', ''))
                           for s in steps[index + 1:]):
                    problems.append(f'{where} does not retain {logs}/ when it fails')
        if found != count:
            problems.append(f'{workflow}: expected {count} run(s) of {producer}, found {found}')
print('\n'.join(problems))
raise SystemExit(bool(problems))
PY
); then ok
else bad "every fixture producer is bounded by the process watchdog:
$problems"; fi

printf '  %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
