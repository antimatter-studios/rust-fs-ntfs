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
PY
echo 'win-write-compressed: ok (the op and its script agree, and the script compresses with compact /c)'
