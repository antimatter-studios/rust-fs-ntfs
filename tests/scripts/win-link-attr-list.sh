#!/usr/bin/env bash
# Windows links one file past what its base record holds and records which
# attributes it moved out to extension records (#1067): the scenario, its op
# and the CI step that runs it must agree, and the op must fail when Windows
# left the file without an $ATTRIBUTE_LIST.
set -euo pipefail
cd "$(dirname "$0")/../.."
python3 - <<'CHECK'
import json
import re
import tomllib
from pathlib import Path
matrix = json.loads(Path('test-matrix.json').read_text())
scenarios = matrix.get('scenarios', matrix)
name = 'win-format-win-link-attr-list-win-chkdsk'
assert name in scenarios, f'{name} is not in test-matrix.json'
scenario = scenarios[name]
assert scenario['status'] == 'pending', 'the scenario has not run: its status is pending'
recipe = scenario['recipe']
assert [step['op'] for step in recipe] == [
    'init-image', 'ship-to-vm', 'win-format', 'win-link-attr-list', 'ship-to-host', 'win-chkdsk'
], 'Windows formats, links, ships the image home as the fixture, then grades it'
link = recipe[3]
assert link['path'] == '/many.txt'
assert link['link_dir'] == '/e'
assert int(link['links']) > 32, 'the 33rd link is the one this driver refuses'
assert link['keep_image'] == 'true'
assert recipe[5]['verdict_shape'] == 'Clean'
config = tomllib.loads(Path('fs-windows-test-harness.toml').read_text())
op = config['ops'].get('win-link-attr-list')
assert op is not None, 'fs-windows-test-harness.toml has no [ops.win-link-attr-list]'
assert op['expect_exit'] == 0
command = op['command']
script = 'scripts/fs-windows-test-harness/win-link-attr-list.ps1'
assert script in command
for flag, field in (('-Path', 'path'), ('-LinkDir', 'link_dir'), ('-Links', 'links')):
    assert f"{flag} '{{step.{field}}}'" in command, f'{flag} is not passed from step.{field}'
source = Path(script).read_text()
assert 'FSCTL_GET_NTFS_FILE_RECORD' in source, 'the op reads the record Windows wrote'
assert 'attr-list.txt' in source, 'the op records what it measured'
assert re.search(r'0x20\b', source), 'the op looks for $ATTRIBUTE_LIST (type 0x20)'
assert 'no $ATTRIBUTE_LIST' in source, 'the op fails when Windows made no $ATTRIBUTE_LIST'
assert 'win-link-attr-list.ps1' in Path('scripts/fs-windows-test-harness/README.md').read_text()
ci = Path('.github/workflows/ci.yml').read_text()
assert f'scripts/run-matrix.sh {name}' in ci, 'CI does not run the scenario'
print('win-link-attr-list: ok')
CHECK
