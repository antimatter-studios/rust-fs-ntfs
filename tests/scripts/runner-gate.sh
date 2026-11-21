#!/usr/bin/env bash
# Every job that may run on our hardware takes its runner from a gate job on the
# runner gate (antimatter-vps-gate), and the gate job is built to the one shape the
# gate accepts. Run against every workflow, so a job added later is held to it.
#
#   <job>-gate:  needs and if as <job>, runs-on antimatter-vps-gate,
#                timeout-minutes 1, one `runner-gate pick <class>` step per runner
#   <job>:       needs the gate, runs-on ${{ needs.<job>-gate.outputs.runner }}
#
# A job with `if: always()` (or `!cancelled()`) gives its gate the same condition and
# keeps its old runner as the fallback, since a skipped gate has no output.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
python3 - "$ROOT" <<'PY'
import re
import sys
from pathlib import Path

import yaml

root = Path(sys.argv[1])
CLASSES = {"linux-x64", "linux-x64-kvm", "linux-arm64", "macos-arm64", "windows-x64", "windows-arm64"}
GATE_LABEL = "antimatter-vps-gate"
EXPR = re.compile(r"\$\{\{\s*needs\.([A-Za-z0-9_-]+)\.outputs(?:\.runner|\[matrix\.os\])(?:\s*\|\|\s*'([^']+)')?\s*\}\}$")


def listed(value):
    return [] if value is None else [value] if isinstance(value, str) else list(value)


problems = []
checked = jobs_total = 0
for path in sorted((root / ".github/workflows").glob("*.yml")):
    text = path.read_text()
    assert "runner-gate.antimatter-studios.com" not in text, f"{path.name}: the old HTTP picker is back"
    assert not re.search(r"^\s+runners:\s", text, re.M), f"{path.name}: a `runners` output is back"
    workflow = yaml.load(text, Loader=yaml.BaseLoader)
    jobs = workflow["jobs"]
    for name, job in jobs.items():
        jobs_total += 1
        where = f"{path.name}:{name}"
        if "uses" in job:
            # A reusable workflow's caller has no runner of its own to pick.
            assert "runs-on" not in job, where
            continue
        runs_on = job.get("runs-on", "")
        if runs_on == GATE_LABEL:
            assert name.endswith("-gate") or name == "gate", f"{where}: a gate job must be named gate or <job>-gate"
            assert job.get("timeout-minutes") == "1", f"{where}: a gate job takes 1 minute"
            steps = job["steps"]
            assert steps, where
            for step in steps:
                assert re.fullmatch(r"runner-gate pick (\S+)", step["run"].strip()), f"{where}: step is not `runner-gate pick <class>`"
                assert step["run"].split()[-1] in CLASSES, f"{where}: unknown class {step['run']}"
                assert step.get("id", "").startswith("pick"), f"{where}: pick steps are named pick*"
            ids = {s["id"] for s in steps}
            for output, value in job["outputs"].items():
                match = re.fullmatch(r"\$\{\{ steps\.([a-z0-9-]+)\.outputs\.runner \}\}", value)
                assert match and match.group(1) in ids, f"{where}: output {output} does not read a pick step"
            continue
        match = EXPR.fullmatch(runs_on)
        if not match:
            # A job that stays where it is must not be one the gate could place.
            assert not re.search(r"ubuntu-|macos-|windows-", runs_on), \
                f"{where}: runs-on {runs_on!r} is a hosted runner that no gate picks"
            continue
        gate_name, fallback = match.groups()
        checked += 1
        assert gate_name == f"{name}-gate", f"{where}: takes its runner from {gate_name}"
        gate = jobs[gate_name]
        assert gate["runs-on"] == GATE_LABEL, f"{where}: its gate does not run on {GATE_LABEL}"
        assert gate_name in listed(job.get("needs")), f"{where}: does not need its gate"
        assert [n for n in listed(job.get("needs")) if n != gate_name] == listed(gate.get("needs")), \
            f"{where}: its gate needs something else"
        assert job.get("if") == gate.get("if"), f"{where}: its gate has another condition"
        condition = job.get("if", "")
        if re.search(r"always\(\)|!cancelled\(\)", condition):
            assert fallback, f"{where}: a job that runs after a failure needs a fallback runner"
            assert fallback in {"ubuntu-24.04", "ubuntu-latest", "windows-latest"}, f"{where}: fallback {fallback}"
        else:
            assert not fallback, f"{where}: only an always() job has a fallback"
        if "[matrix.os]" in runs_on:
            legs = set(job["strategy"]["matrix"]["os"])
            assert set(gate["outputs"]) == legs, f"{where}: one gate output per matrix leg"
        else:
            assert set(gate["outputs"]) == {"runner"}, where

# The classes: where a job was on a hosted image, the class says what it needs.
ci = yaml.load((root / ".github/workflows/ci.yml").read_text(), Loader=yaml.BaseLoader)["jobs"]


def picked(workflow, gate):
    return [s["run"].split()[-1] for s in workflow[gate]["steps"]]


# Each native Linux architecture is its own job with its own gate: a gate picks one runner.
assert picked(ci, "test-linux-x64-gate") == ["linux-x64"], "the x86_64 job runs on an x86_64 runner"
assert picked(ci, "test-linux-arm64-gate") == ["linux-arm64"], "the ARM job stays native ARM"
for gate in ("test-linux-x64-gate", "test-linux-arm64-gate"):
    assert set(ci[gate]["outputs"]) == {"runner"}, gate
for job in ("windows-native-read-fixtures", "windows-4kn-fixture", "op-sequence-chkdsk", "validate-mkfs-windows-run"):
    assert picked(ci, f"{job}-gate") == ["windows-x64"], job
for job in ("integration", "cli", "kernel", "box-cache"):
    assert picked(ci, f"{job}-gate") == ["linux-x64-kvm"], job
for job in ("changes", "interrupted-bootstrap", "python-abi", "readonly-file-name", "darwin-cross", "asan", "semver"):
    assert picked(ci, f"{job}-gate") == ["linux-x64"], job
# ci-ok and the Windows verdict run after everything, whatever happened to it.
for job in ("ci-ok", "validate-mkfs-windows"):
    assert ci[f"{job}-gate"]["if"] == "always()" and ci[job]["if"] == "always()", job
assert "ci-ok-gate" in listed(ci["ci-ok"]["needs"]), "ci-ok takes its runner from ci-ok-gate"
print(f"runner gate: {checked} of {jobs_total} jobs take their runner from a gate job, all built to one shape")
PY
