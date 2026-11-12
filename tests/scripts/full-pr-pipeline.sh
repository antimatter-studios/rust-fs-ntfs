#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
python3 - "$ROOT" <<'PY'
import copy
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

try:
    import yaml
except ImportError:
    raise SystemExit("full-pr-pipeline: PyYAML is required; install it before chore test:scripts")

root = Path(sys.argv[1])
required_workflows = (
    "ci.yml",
    "format-geometry.yml",
    "hardlink-readonly-index-oracle.yml",
    "logfile-nonzero-links.yml",
    "python-directory-abi.yml",
    "security-descriptor-oracle.yml",
    "slate-oracle.yml",
)
workflows = {
    name: yaml.load((root / ".github/workflows" / name).read_text(), Loader=yaml.BaseLoader)
    for name in required_workflows
}


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def validate(configuration):
    for name, workflow in configuration.items():
        triggers = workflow.get("on", {})
        require("pull_request" in triggers, f"{name}: pull_request trigger is missing")
        require(triggers["pull_request"] in (None, "", {}), f"{name}: PR trigger must be unfiltered")
        for job_name, job in workflow["jobs"].items():
            if name == "ci.yml" and job_name == "box-cache":
                continue
            require(job.get("if") in (
                None, "always()", "needs.changes.outputs.code == 'true'",
                "needs.changes.outputs.mkfs == 'true'",
            ), f"{name}/{job_name}: validation job has an unexpected gate")
            for step in job.get("steps", []):
                condition = step.get("if", "")
                require("github.event_name" not in condition and "github.ref" not in condition,
                        f"{name}/{job_name}: validation step has an event/ref gate")
    jobs = configuration["ci.yml"]["jobs"]
    for job_name in ("test", "windows-native-read-fixtures", "integration", "cli", "kernel", "asan",
                     "validate-mkfs-windows-run", "validate-mkfs-windows", "ci-ok"):
        require(job_name in jobs, f"ci.yml: full-pipeline job {job_name} is missing")
    matrix = jobs["test"]["strategy"]["matrix"]
    require(matrix["os"] == ["ubuntu-latest", "ubuntu-24.04-arm", "macos-latest"],
            "ci.yml: Linux, ARM64 and macOS must all run")
    allowed_exclusion = "${{ fromJSON(needs.changes.outputs.code == 'false' && '[{\"os\":\"ubuntu-24.04-arm\"},{\"os\":\"macos-latest\"}]' || '[]') }}"
    require(matrix.get("exclude") in (None, allowed_exclusion), "ci.yml: unexpected platform exclusion")
    changes = jobs["changes"]
    for output in ("code", "mkfs"):
        require(changes["outputs"][output] == "${{ steps.code.outputs.code }}",
                f"ci.yml: {output} must follow the full-pipeline planner")
    planner = next(step for step in changes["steps"] if step.get("id") == "code")
    require(planner["run"] == "bash scripts/ci-plan.sh", "ci.yml: planner is not executed")
    require(planner["env"]["EVENT"] == "${{ github.event_name }}", "ci.yml: planner event is not bound")
    require(planner["env"]["REF"] == "${{ github.ref }}", "ci.yml: planner ref is not bound")


validate(workflows)
rejected = 0
for mutation in ("missing-trigger", "path-filter", "branch-filter", "job-gate", "step-gate",
                 "windows-filter", "missing-fixtures", "missing-platform"):
    changed = copy.deepcopy(workflows)
    jobs = changed["ci.yml"]["jobs"]
    if mutation == "missing-trigger":
        changed["slate-oracle.yml"]["on"].pop("pull_request")
    elif mutation == "path-filter":
        changed["python-directory-abi.yml"]["on"]["pull_request"] = {"paths": ["src/**"]}
    elif mutation == "branch-filter":
        changed["ci.yml"]["on"]["pull_request"] = {"branches": ["main"]}
    elif mutation == "job-gate":
        jobs["integration"]["if"] = "github.event_name != 'pull_request'"
    elif mutation == "step-gate":
        jobs["test"]["steps"][0]["if"] = "github.event_name != 'pull_request'"
    elif mutation == "windows-filter":
        jobs["changes"]["outputs"]["mkfs"] = "${{ steps.filter.outputs.mkfs }}"
    elif mutation == "missing-fixtures":
        jobs.pop("windows-native-read-fixtures")
    elif mutation == "missing-platform":
        jobs["test"]["strategy"]["matrix"]["os"].remove("macos-latest")
    try:
        validate(changed)
    except AssertionError:
        rejected += 1
    else:
        raise AssertionError(f"regression was accepted: {mutation}")

for ref in ("refs/pull/1/merge", "refs/pull/2/merge", "refs/heads/docs-only", "refs/heads/main", ""):
    with tempfile.NamedTemporaryFile() as output:
        environment = dict(os.environ, EVENT="pull_request", REF=ref, GITHUB_OUTPUT=output.name)
        subprocess.run(["bash", str(root / "scripts/ci-plan.sh")], env=environment, check=True,
                       stdout=subprocess.DEVNULL)
        require(Path(output.name).read_text() == "code=true\n", f"PR {ref}: planner is not full")

needs = {name: {"result": "success"} for name in workflows["ci.yml"]["jobs"]["ci-ok"]["needs"]}
environment = dict(os.environ, EVENT="pull_request", REF="refs/pull/1/merge", CODE="true")
command = ["bash", str(root / "scripts/ci-verdict.sh"), "aggregate"]
environment["GATE_NEEDS_JSON"] = json.dumps(needs)
subprocess.run(command, env=environment, check=True, stdout=subprocess.DEVNULL)
for job_name in needs:
    for result in ("skipped", "failure", "cancelled"):
        changed = copy.deepcopy(needs)
        changed[job_name]["result"] = result
        environment["GATE_NEEDS_JSON"] = json.dumps(changed)
        verdict = subprocess.run(command, env=environment, stdout=subprocess.DEVNULL,
                                 stderr=subprocess.DEVNULL)
        require(verdict.returncode != 0, f"PR gate accepted {job_name}={result}")
        rejected += 1

print(f"full PR pipeline: {len(workflows)} unfiltered workflows, 3 platforms, {rejected} rejected regressions")
PY
