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
match = re.search(r'\$type -eq (\S+)\)', source)
assert match, 'Get-Attributes does not test for the end marker'
assert match.group(1) != '0xFFFFFFFF', (
    'PowerShell reads 0xFFFFFFFF as Int32 -1, so the end marker never matches the UInt32 type '
    'and the walk reads the bytes after it as a length')
print('win-link-attr-list: ok')
CHECK

# The walk stops at the end marker, not on the bytes behind it: on Windows the
# record's last eight bytes are 0xFFFFFFFF and whatever the record held before.
if ! command -v pwsh >/dev/null 2>&1; then
    echo 'FAIL  pwsh is required to run the attribute walk: install PowerShell on the runner' >&2
    exit 1
fi
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
python3 - "$tmp/walk.ps1" <<'EXTRACT'
import re
import sys
from pathlib import Path
source = Path('scripts/fs-windows-test-harness/win-link-attr-list.ps1').read_text()
parts = [re.search(r'^function U16\b.*$', source, re.M).group(0),
         re.search(r'^function U32\b.*$', source, re.M).group(0),
         re.search(r'^function Get-Attributes\b.*?^}$', source, re.M | re.S).group(0)]
driver = '''
$ErrorActionPreference = 'Stop'
$PSStyle.OutputRendering = 'PlainText'
$rec = [byte[]]::new(1024)
[BitConverter]::GetBytes([uint16]0x38).CopyTo($rec, 0x14)
[BitConverter]::GetBytes([uint32]0xA0).CopyTo($rec, 0x18)
[BitConverter]::GetBytes([uint32]0x10).CopyTo($rec, 0x38)
[BitConverter]::GetBytes([uint32]0x60).CopyTo($rec, 0x3C)
([byte[]](0xFF, 0xFF, 0xFF, 0xFF, 0x82, 0x79, 0x47, 0x11)).CopyTo($rec, 0x98)
$attrs = Get-Attributes $rec
if ($attrs.Count -ne 1 -or $attrs[0].Type -ne 0x10) { throw "walked $($attrs.Count) attributes" }
'walk: ok'
'''
Path(sys.argv[1]).write_text('\n'.join(parts) + '\n' + driver)
EXTRACT
if ! pwsh -NoProfile -File "$tmp/walk.ps1" >"$tmp/walk.out" 2>&1; then
    echo 'FAIL  the attribute walk reads past the end marker:' >&2
    cat "$tmp/walk.out" >&2
    exit 1
fi
echo 'win-link-attr-list walk: ok'
