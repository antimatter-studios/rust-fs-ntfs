#!/usr/bin/env bash
# The win-write-compressed op and its script agree on their arguments: every
# switch the op's command passes is a parameter the script declares, every
# mandatory parameter is passed, and the script compresses with `compact /c`
# and keeps what Windows says about each file as evidence (#1097).
set -euo pipefail

cd "$(dirname "$0")/../.."
python3 - <<'PY'
import re
import shlex
import tomllib
import json
from pathlib import Path

config = tomllib.loads(Path('fs-windows-test-harness.toml').read_text())
op = config['ops'].get('win-write-compressed')
assert op is not None, 'fs-windows-test-harness.toml declares no [ops.win-write-compressed]'
assert op.get('host') == 'vm', f"win-write-compressed runs on {op.get('host')!r}, not the vm"
assert op.get('expect_exit') == 0, 'win-write-compressed does not expect exit 0'

args = shlex.split(op['command'])
script_arg = args[args.index('-File') + 1]
prefix = '{vm.workdir}/'
assert script_arg.startswith(prefix), f'script {script_arg!r} is not under the vm workdir'
script = Path(script_arg[len(prefix):])
assert script.is_file(), f'{script} does not exist'

passed = {a[1:] for a in args[args.index('-File') + 2:] if a.startswith('-')}
for want in ('ImagePath', 'Files', 'Mode', 'KeepImage', 'Diag'):
    assert want in passed, f'win-write-compressed does not pass -{want}'

text = script.read_text()
block = re.search(r'^param\((.*?)^\)', text, re.S | re.M)
assert block, f'{script} has no param() block'
declared = {}
for line in block.group(1).splitlines():
    m = re.search(r"\$(\w+)\s*(=[^=]*)?,?\s*$", line)
    if m:
        declared[m.group(1)] = 'Mandatory=$true' in line
for name in passed:
    assert name in declared, f'the op passes -{name}, which {script} does not declare'
for name, mandatory in declared.items():
    assert not mandatory or name in passed, f'{script} requires -{name}, which the op does not pass'

assert re.search(r"'compact'.*'/c'|compact(\.exe)? /c", text), f'{script} never compresses with compact /c'
assert re.search(r"'compact'.*'/q'|compact(\.exe)? /q", text), f'{script} keeps no compact /q evidence'
assert "'file', 'layout'" in text, f'{script} keeps no fsutil file layout evidence'
assert 'Compressed' in text, f'{script} never checks the compressed attribute'

name = 'win-format-win-quota-compressed-win-chkdsk'
scenario = json.loads(Path('test-matrix.json').read_text())['scenarios'][name]
recipe = scenario['recipe']
ops = [step['op'] for step in recipe]
assert ops.index('win-quota-track') < ops.index('win-write-compressed') < ops.index('win-chkdsk')
writes = [step for step in recipe if step['op'] == 'win-write-compressed']
assert [step['mode'] for step in writes] == ['write', 'verify']
assert writes[0]['files'] == writes[1]['files'] and writes[0]['files'], 'verification must read the files Windows wrote'
assert ops.index('win-chkdsk') < ops.index('win-quota-verify')
ci = Path('.github/workflows/ci.yml').read_text()
assert f'bash scripts/run-matrix.sh {name}' in ci, 'Windows CI does not run the compressed quota scenario'
assert "throw \"$Tool $($ToolArgs -join ' ') exited" in text[text.index('function Show-Tool'):], 'native evidence failures must fail the scenario'
PY
echo 'win-write-compressed: ok (the op and its script agree, and the script compresses with compact /c)'
