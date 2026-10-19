#!/usr/bin/env bash
# The advisory Slate workflow collects a reliability window (#883, part of
# #694): `slate-oracle.sh window` admits a case only when every run of the
# window agreed on it, so the workflow must run often enough to fill a window
# and keep each run's verdicts until the window is complete. A weekly
# schedule took twenty weeks for twenty runs, and a 14-day artifact expired
# the earliest runs long before that. This reads slate-oracle.yml as data:
# its schedule fires every day (scheduled runs are always on main), and the
# slate-oracle-verdicts-* artifact is kept at least 30 days. The checker is
# also run on copies that must be refused, so a guard that cannot fail is
# caught too.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fails=0

python3 - "$ROOT" <<'PY' || fails=$((fails + 1))
import re, sys
from pathlib import Path
root = Path(sys.argv[1])
MIN_RETENTION_DAYS = 30

def problems_in(workflow):
    problems = []
    trigger = re.search(r"\n  schedule:\n((?:    - cron: .*\n)+)", workflow)
    if not trigger:
        problems.append("the workflow has no schedule trigger")
    else:
        crons = re.findall(r"cron: '([^']*)'", trigger.group(1))
        daily = []
        for cron in crons:
            fields = cron.split()
            if len(fields) != 5:
                problems.append(f"cron {cron!r} does not have five fields")
                continue
            minute, hour, dom, month, dow = fields
            if minute.isdigit() and hour.isdigit() and (dom, month, dow) == ("*", "*", "*"):
                daily.append(cron)
        if not daily:
            problems.append(f"no cron in {crons} fires once every day")
    step = re.search(
        r"\n      - uses: actions/upload-artifact@v\d+\n(?:        .*\n)*?"
        r"          name: slate-oracle-verdicts-\$\{\{ github\.sha \}\}\n((?:          .*\n)*)",
        workflow,
    )
    if not step:
        problems.append("no step uploads the slate-oracle-verdicts-* artifact")
    else:
        days = re.search(r"\n          retention-days: (\d+)\n", "\n" + step.group(1))
        if not days:
            problems.append("the verdicts artifact names no retention-days")
        elif int(days.group(1)) < MIN_RETENTION_DAYS:
            problems.append(
                f"the verdicts artifact is kept {days.group(1)} days, under {MIN_RETENTION_DAYS}"
            )
    path = "tests/scripts/slate-oracle-schedule.sh"
    if f"      - {path}\n" not in workflow:
        problems.append(f"a change to {path} does not run the workflow")
    return problems

workflow = (root / ".github/workflows/slate-oracle.yml").read_text()
problems = problems_in(workflow)

# Each mutation must be refused. Each is applied only where its target text
# exists, so a mutation that no longer matches is itself a failure.
verdicts = re.compile(
    r"(name: slate-oracle-verdicts-\$\{\{ github\.sha \}\}\n(?:          .*\n)*?"
    r"          retention-days: )\d+"
)
crons = re.compile(r"(    - cron: ')[^']*'")
mutations = {
    "a weekly schedule": (crons, r"\g<1>17 4 * * 1'"),
    "a monthly schedule": (crons, r"\g<1>17 4 1 * *'"),
    "an hour list on a weekly day": (crons, r"\g<1>17 4,16 * * 1'"),
    "a 14-day verdicts artifact": (verdicts, r"\g<1>14"),
    "no schedule trigger": (re.compile(r"\n  schedule:\n(?:    - cron: .*\n)+"), "\n"),
}
for name, (pattern, replacement) in mutations.items():
    mutated, count = pattern.subn(replacement, workflow)
    if count == 0:
        problems.append(f"the {name!r} mutation found nothing to change")
    elif not problems_in(mutated):
        problems.append(f"the checker accepts a workflow with {name}")

for problem in problems:
    print(f"FAIL {problem}")
sys.exit(1 if problems else 0)
PY

if [ "$fails" -gt 0 ]; then
    echo "slate-oracle-schedule.sh: $fails of 1 checks failed"
    exit 1
fi
echo "slate-oracle-schedule.sh: all checks passed"
