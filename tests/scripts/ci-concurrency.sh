#!/usr/bin/env bash
# A new head cancels the obsolete runs of the same workflow and pull request.
# Pushes to main, merge groups, scheduled and manual runs, and `red:`
# evidence runs each get a group of their own, so nothing cancels or replaces
# them (#1360, #1403).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
python3 - "$ROOT" <<'PY'
from pathlib import Path
import sys

try:
    import yaml
except ImportError:
    raise SystemExit("ci-concurrency: PyYAML is required; install it before chore test:scripts")

SUPERSEDABLE = (
    "github.event_name == 'pull_request' && "
    "!startsWith(github.event.pull_request.title, 'red:') && "
    "!startsWith(github.head_ref, 'red/')"
)
EXPECTED = {
    "group": "${{ github.workflow }}-${{ " + SUPERSEDABLE
    + " && format('pr-{0}', github.event.pull_request.number)"
    + " || format('run-{0}', github.run_id) }}",
    "cancel-in-progress": "${{ " + SUPERSEDABLE + " }}",
}
# Runs once when a pull request closes; there is no head to supersede.
EXEMPT = {"branch-reap.yml"}

checked = []
failures = []
for path in sorted((Path(sys.argv[1]) / ".github/workflows").glob("*.yml")):
    workflow = yaml.load(path.read_text(), Loader=yaml.BaseLoader)
    if "pull_request" not in workflow.get("on", {}) or path.name in EXEMPT:
        continue
    checked.append(path.name)
    if workflow.get("concurrency") != EXPECTED:
        failures.append(f"{path.name}: workflow concurrency is {workflow.get('concurrency')!r}")
    for name, job in workflow["jobs"].items():
        if "concurrency" in job and "cancel-in-progress" in job["concurrency"]:
            failures.append(f"{path.name}/{name}: a job overrides the cancellation policy")

if "ci.yml" not in checked:
    failures.append("ci.yml was not checked")
if failures:
    raise SystemExit("ci-concurrency:\n  " + "\n  ".join(failures))
print(f"ci-concurrency.sh: all checks passed ({len(checked)} workflows)")
PY
