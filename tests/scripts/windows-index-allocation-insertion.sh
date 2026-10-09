#!/usr/bin/env bash
# The wide Windows directory must accept insertion without losing names.
set -euo pipefail
cd "$(dirname "$0")/../.."
python3 - <<'CHECK'
import json
import tomllib
from pathlib import Path
matrix = json.loads(Path('test-matrix.json').read_text())
scenarios = matrix.get('scenarios', matrix)
name = 'win-format-win-write-many-mac-insert-index-allocation-win-chkdsk'
assert name in scenarios, 'wide-directory insertion still expects obsolete refusal'
recipe = scenarios[name]['recipe']
config = tomllib.loads(Path('fs-windows-test-harness.toml').read_text())
assert len(recipe) == 9
assert recipe[3]['op'] == 'win-write-many-size'
assert recipe[3]['count'] == '256'
assert recipe[3]['name_pattern'] == '/file_{N}.txt'
assert recipe[5]['op'] == 'mac-touch'
assert config['ops'][recipe[5]['op']]['expect_exit'] == 0
assert recipe[5]['basename'] == 'host-inserted.txt'
assert recipe[7]['op'] == 'win-chkdsk'
assert recipe[7]['modes'] == 'readonly,/scan'
assert recipe[7]['verdict_shape'] == 'Clean'
assert recipe[8]['op'] == 'win-enumerate'
expected = {f'file_{i:03d}.txt' for i in range(256)} | {'host-inserted.txt'}
assert set(recipe[8]['required_paths'].split(',')) == expected
print('windows index allocation insertion: ok')
CHECK
