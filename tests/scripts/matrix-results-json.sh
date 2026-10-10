#!/usr/bin/env bash
# Structured runner results define cases; parallel console output can interleave.
set -euo pipefail
cd "$(dirname "$0")/../.."
python3 - <<'CHECK'
import importlib.util
import json
import tempfile
from pathlib import Path
spec = importlib.util.spec_from_file_location('matrix_json', 'scripts/_matrix-build-json.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
with tempfile.TemporaryDirectory() as tmp:
    results = Path(tmp) / 'results.json'
    rows = [{'name': 'one', 'status': 'passed'}, {'name': 'two', 'status': 'failed'},
            {'name': 'three', 'status': 'errored'}]
    results.write_text(json.dumps(rows))
    scenarios = module.parse_scenarios(results)
    assert scenarios == {'one': {'status': 'ok'}, 'two': {'status': 'FAILED'},
                         'three': {'status': 'FAILED'}}, 'console parsing lost structured results'
    module.merge_verdicts(scenarios, {'one': {'verdict_shape': 'clean', 'exits': {'readonly': 0}},
                                     'stale-case': {'verdict_shape': 'clean'}})
    assert set(scenarios) == {'one', 'two', 'three'}, 'stale VM verdict invented a case'
    assert scenarios['one']['exits'] == {'readonly': 0}
    for invalid in ({}, [], rows + [rows[0]], [{'name': 'bad', 'status': 'ignored'}],
                    [{'name': 'bad'}], [{'name': None, 'status': 'passed'}]):
        results.write_text(json.dumps(invalid))
        try:
            module.parse_scenarios(results)
        except ValueError:
            pass
        else:
            raise AssertionError(f'invalid runner results accepted: {invalid!r}')
    results.unlink()
    try:
        module.parse_scenarios(results)
    except FileNotFoundError:
        pass
    else:
        raise AssertionError('missing runner results silently accepted')
collector = Path('scripts/_matrix-collect-vm.sh').read_text()
assert '--runner-results "$repo_root/test-diagnostics/matrix/results.json"' in collector
assert "-Root '$VM_WORKDIR/diag'" in collector, 'collector still reads an unrelated default VM directory'
print('matrix results JSON: ok')
CHECK
