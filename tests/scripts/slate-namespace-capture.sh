#!/usr/bin/env bash
# The Slate tier captures one existing matrix recipe per namespace family --
# mkdir, rename, unlink and rmdir -- before Windows mounts its image (#828),
# beside the data writes of #827, and its floors count every captured case.
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

step = re.search(r"run-host-steps \\\n(.*?)>tmp/logs/slate-writer-capture\.log", workflow, re.S)
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

def commands(name):
    """The fs.ntfs subcommands the scenario's cli-oracle verbs run."""
    found = set()
    for step in scenarios.get(name, {}).get("recipe", []):
        verb = re.match(r"bash scripts/cli-oracle\.sh (\S+) ", ops.get(step.get("op"), {}).get("command", ""))
        if not verb:
            continue
        body = re.search(rf"\n    {re.escape(verb.group(1))}\)\n(.*?)\n        ;;\n", oracle, re.S)
        if body:
            found |= set(re.findall(r'run fs "\$image" (\w+)', body.group(1)))
    return found

data = ["cli-populate-win-verify-chkdsk", "cli-write-at-win-verify-chkdsk", "cli-truncate-win-verify-chkdsk"]
for name in data:
    if name not in captured:
        problems.append(f"the #827 data case {name} is no longer captured")
for name in captured:
    if name not in scenarios:
        problems.append(f"{name} is not a matrix scenario")

# Each family needs its own scenario: one recipe that happens to run mkdir
# while populating does not stand in for the namespace family.
families = {"mkdir": "mkdir", "rename": "mv", "unlink": "rm", "rmdir": "rmdir"}
pool = [n for n in captured if n not in data]
covered = {n: commands(n) for n in pool}
if not any(all(families[f] in covered[n] for f, n in zip(families, pick))
           for pick in itertools.permutations(pool, len(families))):
    problems.append("no distinct captured scenario per namespace family "
                    f"{sorted(families)}; captured beyond #827: {pool}")

for p in problems:
    print(f"FAIL {p}")
sys.exit(1 if problems else 0)
PY

if [ "$fails" -gt 0 ]; then
    echo "slate-namespace-capture.sh: $fails checks failed"
    exit 1
fi
echo 'slate-namespace-capture.sh: all checks passed'
