#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
command -v pwsh >/dev/null || { echo 'FAIL: install PowerShell with chore setup:tools' >&2; exit 1; }
python3 "$root/tests/python/test_fixture_process.py"
python3 - "$root" <<'PY'
import pathlib
import re
import sys

import yaml

# The 4Kn build stalled the hosted runner itself (#1352): the step's own
# timeout-minutes, the process watchdog and every `if: always()` step after
# it never ran, and the job log and artifacts were lost with the job. What
# survived was the job's step list, which records the step in progress. So
# each storage operation is its own process and its own step, in a job of
# its own, and a stall costs that job and names its operation.
PHASES = ['create', 'attach', 'initialize', 'partition', 'format', 'populate', 'fsutil',
          'chkdsk', 'detach', 'attach-readonly', 'export', 'detach-readonly', 'record', 'cleanup']
BUILDER = 'test-disks/build-windows-4kn-fixture.ps1'
LIFECYCLE = 'test-disks/ntfs-4kn-read.lifecycle.log'

root = pathlib.Path(sys.argv[1])
jobs = yaml.safe_load((root / '.github/workflows/ci.yml').read_text())['jobs']
native = jobs['windows-native-read-fixtures']['steps']
assert not any(BUILDER in str(step.get('run', '')) for step in native), \
    'the 4Kn build runs in a job of its own, so a stalled runner cannot hold the native fixtures job'
assert not any('ntfs-4kn-read' in str(step.get('with', {}).get('path', '')) for step in native), \
    'the native fixtures job no longer produces the 4Kn files'
job = jobs.get('windows-4kn-fixture')
assert job, 'the 4Kn build needs a job of its own'
assert 0 < job.get('timeout-minutes', 0) <= 20, 'the 4Kn job needs a short job deadline'
assert 'windows-native-read-fixtures' in job.get('needs', []), '4Kn writes the file set ntfs-cli-read.sha256.tsv names'
assert 'windows-4kn-fixture' in jobs['ci-ok']['needs'], 'ci-ok must judge the 4Kn job'
verdict = (root / 'scripts/ci-verdict.sh').read_text()
full = re.search(r'def full: \[([^\]]*)\]', verdict)
assert full and '"windows-4kn-fixture"' in full.group(1), 'a quick run on main skips the 4Kn job like the other Windows fixture jobs'

script = (root / BUILDER).read_text()
declared = re.search(r'\[ValidateSet\(([^)]*)\)\]\s*\[string\]\$Phase', script)
assert declared and re.findall(r"'([^']+)'", declared.group(1)) == PHASES, \
    'the builder runs exactly one named phase per process'

steps = job['steps']
phase_steps = [(index, step) for index, step in enumerate(steps) if BUILDER in str(step.get('run', ''))]
ran = [re.search(r'-Phase (\S+)', step['run']).group(1) for _, step in phase_steps]
assert ran == PHASES, f'every phase is one step, in order: {ran}'
for _, step in phase_steps:
    run = ' '.join(step['run'].split())
    phase = re.search(r'-Phase (\S+)', run).group(1)
    assert 0 < step.get('timeout-minutes', 0) <= 3, f'{phase} needs a step deadline of at most three minutes'
    process = re.search(r'scripts/run-fixture-process\.py --timeout-seconds (\d+)', run)
    assert process and int(process.group(1)) <= 120, f'{phase} needs an independent process deadline of at most 120 s'
    assert f'--lifecycle {LIFECYCLE}' in run, f'{phase} must name its last retained phase on timeout'
    assert f'--log-dir tmp/4kn-fixture-process/{phase} ' in run, f'{phase} keeps its own child output'
    assert (step.get('if') == 'always()') == (phase == 'cleanup'), f'only cleanup runs after a failed phase, not {phase}'
first, last = phase_steps[0][0], phase_steps[-1][0]
assert any('tests/python/test_fixture_process.py' in str(step.get('run', '')) for step in steps[:first]), \
    'the process watchdog is proven on this Windows runner before it guards the build'
retain = steps[last + 1]
assert retain.get('if') == 'always()', '4Kn diagnostics must survive a failed or timed-out build'
assert retain.get('uses', '').startswith('actions/upload-artifact@'), '4Kn diagnostics are uploaded right after cleanup'
for path in (LIFECYCLE, 'tmp/4kn-fixture-process/', 'tmp/4kn-fixture/'):
    assert path in retain['with']['path'], f'4Kn diagnostics must retain {path}'
assert retain['with'].get('if-no-files-found') == 'error', 'missing 4Kn diagnostics must fail'
assert any('--test fixture_4kn_windows' in str(step.get('run', '')) for step in steps[last + 1:]), \
    'the image is held to what Windows reported after it is built'
print('4Kn workflow: one bounded step per phase, in a job of its own, diagnostics retained')
PY
pwsh -NoProfile -File "$root/tests/scripts/windows-4kn-fixture-phases.ps1"
