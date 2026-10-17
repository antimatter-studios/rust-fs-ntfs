#!/usr/bin/env bash
# The explorer must supervise the process, fail closed and retain evidence.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
python3 - <<'PY'
import importlib.util
import pathlib
import subprocess
import tempfile
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("explorer", "scripts/op-sequence-explorer.py")
explorer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(explorer)
with tempfile.TemporaryDirectory() as directory:
    root = pathlib.Path(directory)
    corpus = root / "corpus"
    corpus.mkdir()
    (corpus / "one.json").write_text('{"version":1,"operations":[]}')
    output = root / "output"

    def clean(command, **kwargs):
        assert "-seed=683" in command and "-max_total_time=2" in command
        assert "-timeout=90" in command and "-rss_limit_mb=1024" in command
        assert kwargs["timeout"] == 97
        kwargs["stdout"].write(b"stat::number_of_executed_units: 4\n")
        kwargs["stdout"].flush()
        return subprocess.CompletedProcess(command, 0)

    with patch.object(explorer.subprocess, "run", side_effect=clean):
        explorer.run(2, 683, corpus, output)
    assert (output / "run.json").is_file()
    for status in (1, 77):
        with patch.object(explorer.subprocess, "run", return_value=subprocess.CompletedProcess([], status)):
            try:
                explorer.run(2, 683, corpus, output)
            except RuntimeError:
                pass
            else:
                raise AssertionError("failed explorer passed")
    with patch.object(explorer.subprocess, "run", side_effect=subprocess.TimeoutExpired([], 97)):
        try:
            explorer.run(2, 683, corpus, output)
        except RuntimeError as error:
            assert "deadline" in str(error)
        else:
            raise AssertionError("timeout passed")
    with patch.object(explorer.subprocess, "run", return_value=subprocess.CompletedProcess([], 0)):
        try:
            explorer.run(2, 683, corpus, output)
        except RuntimeError:
            pass
        else:
            raise AssertionError("zero executed inputs passed")
    try:
        explorer.run(2, 683, root / "missing", output)
    except RuntimeError:
        pass
    else:
        raise AssertionError("missing corpus passed")
print("operation explorer: budgets, seed, exit status, timeout, execution count and missing corpus checked")
PY
