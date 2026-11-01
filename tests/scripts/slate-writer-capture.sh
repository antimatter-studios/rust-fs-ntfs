#!/usr/bin/env bash
# Capture the actual matrix host recipes before any Windows mount (#827).
# Fake operations leave distinguishable bytes and markers; no NTFS reader
# or formatter participates in this runner contract test.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/slate-writer-capture.XXXXXX")"
trap 'rm -rf "$work"' EXIT
fails=0
ok() { :; }
bad() { echo "FAIL $*"; fails=$((fails + 1)); }
mkdir -p "$work/repo/scripts/fs-windows-test-harness"
cp "$ROOT/scripts/fs-windows-test-harness/run-host-steps" "$work/repo/scripts/fs-windows-test-harness/"
runner="$work/repo/scripts/fs-windows-test-harness/run-host-steps"
cat >"$work/repo/fs-windows-test-harness.toml" <<EOF
[ops.seed]
host = "host"
command = "printf before >{image_dir}/{run_id}/{scenario.image}"
[ops.write]
host = "host"
command = "printf writer-bytes >{image_dir}/{run_id}/{scenario.image}"
[ops.fail]
host = "host"
command = "exit 3"
[ops.after]
host = "host"
command = "touch $work/after-ran"
[ops.win-chkdsk]
host = "vm"
command = "touch $work/windows-ran"
EOF
python3 - "$work/repo/test-matrix.json" <<'PY'
import json, sys
ship = {"op": "ship-to-vm"}
grade = {"op": "win-chkdsk", "modes": "readonly,/scan", "verdict_shape": "Clean"}
def case(ops):
    return {"image": "writer.img", "recipe": ops}
good = [{"op": "seed"}, {"op": "write"}, ship, grade]
json.dump({"scenarios": {
    "data": case(good), "second": case(good),
    "failed": case([{"op": "fail"}, ship, grade]),
    "missing": case([ship, grade]),
    "ungraded": case([{"op": "seed"}, ship]),
    "repair": case([{"op": "seed"}, ship, {**grade, "verdict_shape": "RepairRequired"}]),
    "scan-only": case([{"op": "seed"}, ship, {**grade, "modes": "/scan"}]),
    "after": case(good + [{"op": "after"}]),
}}, open(sys.argv[1], "w"))
PY

"$runner" --scenario data --scenario second --capture-dir "$work/capture" --floor 2 >"$work/log" 2>&1
rc=$?
[ "$rc" -eq 0 ] && ok || bad "capture exits $rc: $(cat "$work/log")"
python3 - "$work/capture" <<'PY'
import csv, hashlib, sys
from pathlib import Path
root = Path(sys.argv[1])
with (root / "captures.tsv").open() as f:
    rows = list(csv.DictReader(f, delimiter="\t"))
assert [r["scenario"] for r in rows] == ["data", "second"]
assert len({r["image"] for r in rows}) == 2
for row in rows:
    image = root / "images" / row["image"]
    assert image.read_bytes() == b"writer-bytes"
    assert hashlib.sha256(image.read_bytes()).hexdigest() == row["sha256"]
    assert row["case"] == row["scenario"]
PY
[ "$?" -eq 0 ] && ok || bad 'the manifest retains both pre-mount images and their SHA-256 values'
[ ! -e "$work/windows-ran" ] && ok || bad 'capture ran Windows'

for scenario in failed missing ungraded repair scan-only after unknown; do
    "$runner" --scenario "$scenario" --capture-dir "$work/$scenario" >"$work/log" 2>&1
    rc=$?
    [ "$rc" -ne 0 ] && ok || bad "$scenario capture succeeded"
    grep -q "$scenario" "$work/log" && ok || bad "$scenario failure is not named"
    [ ! -e "$work/$scenario/captures.tsv" ] && ok || bad "$scenario published an incomplete manifest"
done
[ ! -e "$work/after-ran" ] && ok || bad 'capture ran a host mutation after Windows'
"$runner" --scenario data --capture-dir "$work/capture" >"$work/log" 2>&1
[ "$?" -ne 0 ] && ok || bad 'capture overwrote an existing export'
"$runner" --scenario data --scenario data --capture-dir "$work/duplicate" >"$work/log" 2>&1
[ "$?" -ne 0 ] && ok || bad 'duplicate scenarios satisfied capture floor'
"$runner" --scenario data --capture-dir "$work/short" --floor 2 >"$work/log" 2>&1
[ "$?" -ne 0 ] && ok || bad 'one capture satisfied a floor of two'

if [ "$fails" -gt 0 ]; then
    echo "slate-writer-capture.sh: $fails checks failed"
    exit 1
fi
echo 'slate-writer-capture.sh: all checks passed'
