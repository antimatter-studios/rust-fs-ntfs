#!/usr/bin/env bash
# The Windows gate must exercise both index shapes with EAs and streams.
set -euo pipefail
cd "$(dirname "$0")/../.."
python3 - <<'PY'
import json
from pathlib import Path

name = "mac-format-listed-directory-cycle-win-chkdsk"
matrix = json.loads(Path("test-matrix.json").read_text())
scenario = matrix["scenarios"].get(name)
assert scenario is not None, "#818: the listed-directory Windows cycle is missing"
recipe = scenario["recipe"]

def step(op, **fields):
    matches = [s for s in recipe if s["op"] == op
               and all(s.get(k) == v for k, v in fields.items())]
    assert len(matches) == 1, (op, fields, matches)
    return recipe.index(matches[0])

for directory in ("/grown", "/folded"):
    ea = step("mac-write-eas", path=directory, count="1", size_bytes="800", delete_every="0")
    streams = step("mac-write-streams", path=directory, count="130", size_bytes="120", delete_every="0")
    added = step("mac-touch-many", parent=directory, prefix="listed_", count="80")
    spilled = step("mac-assert-directory-layout", path=directory, layout="spilled")
    assert ea < streams < added < spilled
    step("win-verify-eas", path=directory, count="1", size_bytes="800", delete_every="0", keep_image="true")
    step("win-verify-streams", path=directory, count="130", size_bytes="120", delete_every="0", keep_image="true")

removed = step("mac-rm-many", parent="/folded", prefix="listed_", count="80")
folded = step("mac-assert-directory-layout", path="/folded", layout="resident")
assert step("mac-assert-directory-layout", path="/folded", layout="spilled") < removed < folded
assert folded < step("ship-to-vm") < step("win-verify-listed-directories", keep_image="true")
assert recipe[-1] == {"op": "win-chkdsk", "modes": "readonly,/scan",
                      "verdict_shape": "Clean", "keep_image": "false"}
config = Path("fs-windows-test-harness.toml").read_text()
for op in ("mac-assert-directory-layout", "win-verify-listed-directories"):
    assert f"[ops.{op}]" in config, f"{op} is not wired into the harness"
ci = Path(".github/workflows/ci.yml").read_text()
assert f"bash scripts/run-matrix.sh {name} \\" in ci, "the Windows gate never runs the cycle"
print("listed-directory-matrix: all checks passed")
PY
