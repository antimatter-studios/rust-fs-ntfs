#!/usr/bin/env bash
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

python3 - "$ROOT/.github/workflows/ci.yml" <<'PY'
import copy
import shlex
import sys

try:
    import yaml
except ImportError:
    sys.exit("FAIL script-tier-budget: python3 needs PyYAML (pip install pyyaml)")

passed = 0
failed = 0
command = [
    "bash", "../rust-fs-core/scripts/tier.sh", "--refuse-skips", "scripts", "--",
    "scripts/run-script-tests.sh", "tests/scripts",
]


def violations(document):
    steps = document.get("jobs", {}).get("test", {}).get("steps", [])
    shells = [step for step in steps if step.get("name") == "shell tests"]
    errors = []
    if len(shells) != 1:
        errors.append("test must have exactly one shell tests step")
    else:
        try:
            tokens = shlex.split(shells[0].get("run", ""))
        except ValueError:
            tokens = []
        if tokens != command:
            errors.append("shell tests must run tier.sh --refuse-skips scripts -- scripts/run-script-tests.sh tests/scripts")
    uploads = [
        step for step in steps
        if step.get("uses", "").split("@")[0] == "actions/upload-artifact"
        and step.get("if") in ("always()", "${{ always() }}")
        and any(
            path.strip() in ("tmp/logs/", "tmp/logs", "tmp/logs/scripts.log")
            for path in step.get("with", {}).get("path", "").splitlines()
        )
    ]
    if not uploads:
        errors.append("test must upload tmp/logs/scripts.log even on failure")
    return errors


def check(condition, message):
    global passed, failed
    if condition:
        passed += 1
    else:
        failed += 1
        print(f"FAIL script-tier-budget: {message}")


with open(sys.argv[1], encoding="utf-8") as source:
    document = yaml.safe_load(source)
errors = violations(document)
check(not errors, "; ".join(errors))

control = yaml.safe_load("""
jobs:
  test:
    steps:
      - name: shell tests
        run: bash ../rust-fs-core/scripts/tier.sh --refuse-skips scripts -- scripts/run-script-tests.sh tests/scripts
      - uses: actions/upload-artifact@v4
        if: always()
        with:
          path: tmp/logs/
""")
check(not violations(control), "the budgeted command and logs directory must pass")

for label, replacement in (
    ("direct runner", "scripts/run-script-tests.sh tests/scripts"),
    ("missing skip refusal", shlex.join(command).replace("--refuse-skips ", "")),
    ("explicit caps bypassing the row", shlex.join(command).replace("scripts --", "scripts scripts 185 6000 --")),
    ("another tier", shlex.join(command).replace(" scripts --", " unit --")),
    ("another directory", shlex.join(command).replace("tests/scripts", "tests/other")),
    ("masked status", shlex.join(command) + " || true"),
    ("quoted command instead of execution", "echo " + shlex.quote(shlex.join(command))),
    ("a comment instead of the wrapper", "scripts/run-script-tests.sh tests/scripts # " + shlex.join(command)),
):
    mutant = copy.deepcopy(control)
    mutant["jobs"]["test"]["steps"][0]["run"] = replacement
    check(bool(violations(mutant)), f"{label} must fail")

mutant = copy.deepcopy(control)
mutant["jobs"]["test"]["steps"].append(copy.deepcopy(mutant["jobs"]["test"]["steps"][0]))
check(bool(violations(mutant)), "duplicate shell tests steps must fail")

for label, field, value in (
    ("success-only upload", "if", "success()"),
    ("another artifact", "with", {"path": "tmp/logs/unit.log"}),
):
    mutant = copy.deepcopy(control)
    mutant["jobs"]["test"]["steps"][1][field] = value
    check(bool(violations(mutant)), f"{label} must fail")

print(f"script-tier-budget: {passed} passed, {failed} failed")
sys.exit(failed > 0)
PY
