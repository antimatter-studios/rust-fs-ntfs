#!/usr/bin/env bash
# The advisory Slate workflow runs Slate's copy-only writer on copies of the
# captured write-at image (#863, part of #690), from the committed write
# table tests/slate-writes.tsv. Each row's expected bytes are the bytes the
# write-at recipe in cli-oracle.sh leaves at that offset, the floor counts
# every row, and the step uploads the source images, the written copies and
# slate-writes.tsv. This reads the table, the workflow and cli-oracle.sh as
# data; the write step itself is tests/scripts/slate-oracle.sh's contract.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fails=0

python3 - "$ROOT" <<'PY' || fails=$((fails + 1))
import re, sys
from pathlib import Path
root = Path(sys.argv[1])
problems = []

table = root / "tests/slate-writes.tsv"
if not table.is_file():
    sys.exit("FAIL tests/slate-writes.tsv, the write table, does not exist")
lines = table.read_text().splitlines()
header = "case\tsource\troot_file\toffset\texpected_hex\treplacement_hex"
if not lines or lines[0] != header:
    problems.append(f"the write table's header is not {header!r}")
rows = [line.split("\t") for line in lines[1:] if line]

workflow = (root / ".github/workflows/slate-oracle.yml").read_text()
oracle = (root / "scripts/cli-oracle.sh").read_text()
captured = set(re.findall(r"--scenario (\S+)", workflow))

# The write-at recipe: each file holds OLD bytes of Z, then COUNT bytes of Q
# are written at AT, and any gap between OLD and AT reads as zeros.
body = re.search(r"\n    write-at\)\n(.*?)\n        ;;\n", oracle, re.S)
recipe = {}
if body:
    for name, old, at, count in re.findall(r"'(\w+) (\d+) (\d+) (\d+)'", body.group(1)):
        recipe[name] = (int(old), int(at), int(count))
if not recipe:
    problems.append("cli-oracle.sh's write-at recipe no longer lists its files")

def byte(name, offset):
    old, at, count = recipe[name]
    if at <= offset < at + count:
        return ord("Q")
    return ord("Z") if offset < old else 0

seen = set()
for row in rows:
    if len(row) != 6:
        problems.append(f"row {row} does not have six fields")
        continue
    case, source, root_file, offset, expected, replacement = row
    if not re.fullmatch(r"[A-Za-z0-9_-]+", case) or case in seen:
        problems.append(f"case {case!r} is malformed or listed twice")
    seen.add(case)
    if source != "cli-write-at-win-verify-chkdsk.img":
        problems.append(f"{case}: source {source} is not the captured write-at image")
    elif source[:-4] not in captured:
        problems.append(f"{case}: the writer-cases job does not capture {source[:-4]}")
    if root_file not in recipe:
        problems.append(f"{case}: {root_file} is not a file the write-at recipe writes")
        continue
    hexes = re.compile(r"(?:[0-9a-f]{2})+")
    if not offset.isdigit() or not hexes.fullmatch(expected) or not hexes.fullmatch(replacement):
        problems.append(f"{case}: offset or hex fields are malformed")
        continue
    if len(expected) != len(replacement) or expected == replacement:
        problems.append(f"{case}: the replacement is not a different patch of the same length")
    want = bytes(byte(root_file, int(offset) + i) for i in range(len(expected) // 2)).hex()
    if expected != want:
        problems.append(f"{case}: expected {expected}, but write-at leaves {want} at /{root_file}+{offset}")
if not rows:
    problems.append("the write table has no rows")

job = re.search(r"\n  slate-check:\n(.*?)(?=\n  [A-Za-z0-9_-]+:\n|\Z)", workflow, re.S)
job = job.group(1) if job else ""
if not re.search(r'slate-oracle\.sh build "\$RUNNER_TEMP/slate-ntfs" ntfs-write-lab', job):
    problems.append("the slate-check job does not build ntfs-write-lab")
run = re.search(r"slate-oracle\.sh write\s+tests/slate-writes\.tsv\s+tmp/slate-oracle/images\s+"
                r'"\$SLATE_WRITER"\s+tmp/slate-oracle/writes\s+(\d+)', job)
if not run:
    problems.append("the slate-check job does not run the write step on tests/slate-writes.tsv")
elif int(run.group(1)) != len(rows):
    problems.append(f"write floor {run.group(1)} is not the {len(rows)} rows of the write table")
upload = re.search(r"name: slate-oracle-writes-\$\{\{ github\.sha \}\}\n(.*?)if-no-files-found: error", job, re.S)
if not upload:
    problems.append("the slate-check job does not upload slate-oracle-writes")
else:
    for want in ["tmp/slate-oracle/writes/", "tmp/slate-oracle/images/cli-write-at-win-verify-chkdsk.img"]:
        if want not in upload.group(1):
            problems.append(f"the writes artifact does not carry {want}")
for path in ["tests/slate-writes.tsv", "tests/scripts/slate-write-workflow.sh"]:
    if f"      - {path}\n" not in workflow:
        problems.append(f"a change to {path} does not run the workflow")

for problem in problems:
    print(f"FAIL {problem}")
sys.exit(1 if problems else 0)
PY

if [ "$fails" -gt 0 ]; then
    echo "slate-write-workflow.sh: $fails of 1 checks failed"
    exit 1
fi
echo "slate-write-workflow.sh: all checks passed"
