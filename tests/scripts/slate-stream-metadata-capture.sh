#!/usr/bin/env bash
# The Slate tier captures the existing matrix recipes that write stream and
# metadata attributes -- named streams, extended attributes and reparse
# points -- before Windows mounts their images (#829), beside the data writes
# of #827 and the namespace writes of #828, and its floors count every case.
# This reads the workflow, the matrix, the harness and cli-oracle.sh as data;
# the capture itself is slate-writer-capture.sh's contract.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fails=0

python3 - "$ROOT" <<'PY' || fails=$((fails + 1))
import itertools, json, re, sys, tomllib
from pathlib import Path
root = Path(sys.argv[1])
workflow = (root / ".github/workflows/slate-oracle.yml").read_text()
matrix = json.loads((root / "test-matrix.json").read_text())
scenarios = matrix.get("scenarios", matrix)
ops = tomllib.loads((root / "fs-windows-test-harness.toml").read_text()).get("ops", {})
oracle = (root / "scripts/cli-oracle.sh").read_text()
problems = []

job = re.search(r"\n  writer-cases:\n(.*?)\n  [\w-]+:\n", workflow, re.S)
if not job:
    sys.exit("FAIL the slate-oracle workflow has no writer-cases job")
step = re.search(r"run-host-steps \\\n(.*?)>tmp/logs/slate-writer-capture\.log", job.group(1), re.S)
if not step:
    sys.exit("FAIL the writer-cases job no longer runs run-host-steps into slate-writer-capture.log")
captured = re.findall(r"--scenario (\S+)", step.group(1))
floor = re.search(r"--floor (\d+)", step.group(1))
if len(set(captured)) != len(captured):
    problems.append(f"a scenario is captured twice: {captured}")
if not floor or int(floor.group(1)) != len(captured):
    problems.append(f"capture floor {floor and floor.group(1)} is not the {len(captured)} captured scenarios")
compare = re.search(r'"\$SLATE_CHECKER" tmp/slate-oracle/out (\d+)', workflow)
if not compare or int(compare.group(1)) != 4 + len(captured):
    problems.append(f"compare floor {compare and compare.group(1)} is not four format controls "
                    f"plus {len(captured)} captured cases")

# Named streams and EAs are written by the harness binary, which cli-oracle.sh
# refuses to run without; it must be built before the capture runs.
before = job.group(1)[:step.start()]
if not re.search(r"cargo build --locked --release --features harness --bin rust-ntfs", before):
    problems.append("the writer-cases job does not build rust-ntfs with --features harness "
                    "before capturing the stream and EA recipes")

def bodies(name):
    """The cli-oracle.sh verb bodies the scenario's recipe runs."""
    found = []
    for step in scenarios.get(name, {}).get("recipe", []):
        verb = re.match(r"bash scripts/cli-oracle\.sh (\S+) ", ops.get(step.get("op"), {}).get("command", ""))
        if not verb:
            continue
        body = re.search(rf"\n    {re.escape(verb.group(1))}\)\n(.*?)\n        ;;\n", oracle, re.S)
        if body:
            found.append(body.group(1))
    return "\n".join(found)

for name in captured:
    if name not in scenarios:
        problems.append(f"{name} is not a matrix scenario")

# The namespace recipes of #828 stay captured; cli-link-win-verify-chkdsk is
# also the matrix's only symbolic link, so it carries the reparse family.
earlier = ["cli-populate-win-verify-chkdsk", "cli-write-at-win-verify-chkdsk",
           "cli-truncate-win-verify-chkdsk", "cli-mkdir-parents-win-verify-chkdsk",
           "cli-rename-win-verify-chkdsk", "cli-link-win-verify-chkdsk",
           "cli-empty-directory-win-verify-chkdsk"]
for name in earlier:
    if name not in captured:
        problems.append(f"the #827/#828 case {name} is no longer captured")

writes = {
    "named stream": r'"\$HARNESS" stream "\$image" ',
    "extended attribute": r'"\$HARNESS" ea "\$image" ',
    "reparse point": r'run fs "\$image" ln -s ',
}
text = {n: bodies(n) for n in captured}
for family, pattern in writes.items():
    if not any(re.search(pattern, text[n]) for n in captured):
        problems.append(f"no captured scenario writes the {family} family")

# Streams and EAs each need a scenario of their own beyond #827 and #828: one
# recipe that writes both does not stand in for two families.
pool = [n for n in captured if n not in earlier]
pair = [writes["named stream"], writes["extended attribute"]]
if not any(all(re.search(p, text[n]) for p, n in zip(pair, pick))
           for pick in itertools.permutations(pool, len(pair))):
    problems.append("no distinct captured scenario each for named streams and EAs "
                    f"beyond #827 and #828; captured beyond them: {pool}")

if "tests/scripts/slate-stream-metadata-capture.sh" not in workflow:
    problems.append("the stream and metadata capture guard is not in the workflow's path filter")

for p in problems:
    print(f"FAIL {p}")
sys.exit(1 if problems else 0)
PY

if [ "$fails" -gt 0 ]; then
    echo "slate-stream-metadata-capture.sh: $fails checks failed"
    exit 1
fi
echo 'slate-stream-metadata-capture.sh: all checks passed'
