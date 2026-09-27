#!/usr/bin/env bash
# An empty optional value must survive SSH command parsing as a nonempty
# PowerShell argument. The script strips the transport prefix after binding.
set -euo pipefail

cd "$(dirname "$0")/../.."
python3 - <<'PY'
import shlex
import tomllib
from pathlib import Path

config = tomllib.loads(Path('fs-windows-test-harness.toml').read_text())
template = config['ops']['win-enumerate']['command']
for paths in ('', 'fragdir/file_01.txt,fragdir/file_80.txt'):
    command = template.replace('{step.required_paths?}', paths)
    args = shlex.split(command)
    position = args.index('-RequiredPaths')
    value = args[position + 1]
    assert value, f'empty RequiredPaths argument for {paths!r}'
    assert value != '-Diag', f'missing RequiredPaths argument for {paths!r}'
    assert value[1:] == paths, f'RequiredPaths changed in transit: {value!r}'
print('win-enumerate optional paths: ok')
PY
