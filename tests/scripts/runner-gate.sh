#!/usr/bin/env bash
# Execute CI's picker with a fake HTTP client: no network or runner reservation.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
python3 - "$ROOT" <<'PY'
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

import yaml

root = Path(sys.argv[1])
workflow = yaml.load((root / ".github/workflows/ci.yml").read_text(), Loader=yaml.BaseLoader)
jobs = workflow["jobs"]
picker = jobs["changes"]
assert picker["runs-on"] == "ubuntu-latest"
assert not picker.get("needs"), "picker must not depend on a gated job"
assert picker["outputs"]["runners"] == "${{ steps.runners.outputs.runners || '{}' }}"
step = picker["steps"][-1]
assert step["id"] == "runners", "runner picking must be the last step"
identities = {
    "ACTOR": "${{ github.triggering_actor }}",
    "GH_ACTOR": "${{ github.actor }}",
    "SENDER": "${{ github.event.sender.login }}",
    "AUTHOR": "${{ github.event.pull_request.user.login }}",
}
for key, value in identities.items():
    assert step["env"][key] == value, f"identity is not bound: {key}"
assert step["env"]["ORG"] == "${{ github.repository_owner }}"
keys = step["env"]["JOBS"].split()
expected = {
    "interrupted-bootstrap", "python-abi", "readonly-file-name",
    "test-ubuntu-latest", "darwin-cross", "asan", "semver",
}
assert set(keys) == expected and len(keys) == len(expected)
for key in keys:
    job = jobs["test" if key == "test-ubuntu-latest" else key]
    needs = job["needs"]
    assert "changes" in ([needs] if isinstance(needs, str) else needs)
    expression = f"fromJSON(needs.changes.outputs.runners)['{key}'] || 'ubuntu-24.04'"
    assert expression in job["runs-on"], f"missing safe runner lookup for {key}"
for job in ("integration", "cli", "python-volume-info-windows"):
    assert jobs[job]["needs"] == ["changes", "windows-native-read-fixtures"]
    assert "if" not in jobs[job], "fixture skip semantics must not change"
# FUSE mounts (integration) and real man pages (cli) do not work in the
# unprivileged, minimized antimatter-vps pods: these stay on GitHub.
for job in ("integration", "cli"):
    assert jobs[job]["runs-on"] == "ubuntu-24.04", f"{job} must stay hosted"
assert jobs["validate-mkfs-windows"]["if"] == "always()"
# Jobs that wait on a long Windows job pick in that job's last step, so the
# gate's 120 s reservation is still live when they start.
late = {
    "windows-native-read-fixtures": ["python-volume-info-windows"],
    "validate-mkfs-windows-run": ["validate-mkfs-windows"],
}
for pred, late_keys in late.items():
    late_step = jobs[pred]["steps"][-1]
    assert late_step["id"] == "runners" and late_step.get("shell") == "bash"
    assert late_step["env"] == dict(step["env"], JOBS=" ".join(late_keys))
    assert late_step["run"] == step["run"], "all pickers must share one contract"
    assert jobs[pred]["outputs"]["runners"] == "${{ steps.runners.outputs.runners || '{}' }}"
    for key in late_keys:
        expression = f"fromJSON(needs.{pred}.outputs.runners || '{{}}')['{key}'] || 'ubuntu-24.04'"
        assert expression in jobs[key]["runs-on"], f"late pick lookup missing for {key}"
        needs = jobs[key]["needs"]
        assert pred in ([needs] if isinstance(needs, str) else needs)
assert jobs["ci-ok"]["runs-on"] == "ubuntu-latest"
assert jobs["kernel"]["runs-on"] == jobs["box-cache"]["runs-on"] == "ubuntu-24.04"
for job in ("windows-native-read-fixtures", "op-sequence-chkdsk", "validate-mkfs-windows-run"):
    assert jobs[job]["runs-on"] == "windows-latest"
assert jobs["test"]["strategy"]["matrix"]["os"] == ["ubuntu-latest", "ubuntu-24.04-arm"]
assert "|| matrix.os" in jobs["test"]["runs-on"], "ARM must keep its native hosted runner"

script = step["run"].replace("${{ github.run_id }}", "12345").replace("${{ github.run_attempt }}", "2")
assert "${{" not in script, "unbound expression in picker test"
cases = 0
with tempfile.TemporaryDirectory(prefix="runner-gate-") as scratch:
    scratch = Path(scratch)
    curl = scratch / "curl"
    curl.write_text(f"#!{sys.executable}\n" + '''import json, os, sys
from pathlib import Path
with Path(os.environ["CALLS"]).open("a") as output:
    output.write(json.dumps(sys.argv[1:]) + "\\n")
mode = os.environ["GATE_MODE"]
if mode == "error":
    print("antimatter-vps-linux-x64")
    sys.exit(28)
if mode == "mixed":
    job = next(a[4:] for a in sys.argv[1:] if a.startswith("job="))
    print("antimatter-vps-linux-x64" if job == "test-ubuntu-latest" else "ubuntu-24.04")
else:
    print(os.environ["GATE_RESPONSE"], end="")
''')
    curl.chmod(0o755)

    def check(identities, response="antimatter-vps-linux-x64", mode="label", allowed=True):
        global cases
        cases += 1
        calls = scratch / "calls"
        calls.write_text("")
        output = scratch / "output"
        summary = scratch / "summary"
        output.write_text("")
        summary.write_text("")
        environment = dict(os.environ, PATH=f"{scratch}:{os.environ['PATH']}",
                           ORG="antimatter-studios", JOBS=" ".join(keys), CALLS=str(calls),
                           GATE_RESPONSE=response, GATE_MODE=mode,
                           GITHUB_OUTPUT=str(output), GITHUB_STEP_SUMMARY=str(summary),
                           **identities)
        result = subprocess.run(["bash", "-c", script], env=environment, check=True,
                                capture_output=True, text=True)
        runner_map = json.loads(output.read_text().removeprefix("runners="))
        assert json.loads(summary.read_text()) == runner_map == json.loads(result.stdout)
        valid = {"antimatter-vps-linux-x64", "ubuntu-24.04"}
        wanted = response.rstrip("\n") if allowed and mode == "label" and response.rstrip("\n") in valid else "ubuntu-24.04"
        for key in keys:
            label = "antimatter-vps-linux-x64" if allowed and mode == "mixed" and key == "test-ubuntu-latest" else wanted
            assert runner_map[key] == label, (identities, mode, response, key, runner_map[key])
        requests = [json.loads(line) for line in calls.read_text().splitlines()]
        assert len(requests) == (len(keys) if allowed else 0), "one request per job; none for outsiders"
        for key, arguments in zip(keys, requests):
            assert arguments[:5] == ["-fsS", "--max-time", "10", "--get", "https://runner-gate.antimatter-studios.com/pick"]
            query = arguments[5:]
            assert query[::2] == ["--data-urlencode"] * 8
            assert query[1::2] == [
                "org=antimatter-studios", "prefer=antimatter-vps-linux-x64",
                "fallback=ubuntu-24.04", f"actor={identities['ACTOR']}",
                f"author={identities['AUTHOR']}", "run=12345", "attempt=2", f"job={key}",
            ]

    def exercise():
        owner = {key: "christhomas" for key in identities}
        check(owner)
        check(owner, "ubuntu-24.04\n")
        check(owner, "antimatter-vps-linux-x64\n")
        check(owner, mode="mixed")
        check(owner, mode="error")
        for response in ("", "self-hosted", " ubuntu-latest", "ubuntu-latest\nantimatter-vps-linux-x64", "antimatter-vps-linux-x64 "):
            check(owner, response)
        for identity in identities:
            check(dict(owner, **{identity: "outside-user"}), allowed=False)
            check(dict(owner, **{identity: ""}))
        check(dict(owner, SENDER="github-merge-queue[bot]"), allowed=False)
        check({key: "" for key in identities})

    exercise()
    total_picks = len(keys)
    for late_keys in late.values():
        keys = late_keys
        exercise()
        total_picks += len(keys)
    slate = yaml.load((root / ".github/workflows/slate-oracle.yml").read_text(), Loader=yaml.BaseLoader)["jobs"]
    assert "outputs" not in slate["writer-cases"], "slate-check picks after the Windows cases, not before"
    picker = slate["windows-cases"]
    assert picker["runs-on"] == "windows-latest"
    assert picker["outputs"]["runners"] == "${{ steps.runners.outputs.runners || '{}' }}"
    slate_step = picker["steps"][-1]
    assert slate_step["id"] == "runners" and slate_step.get("shell") == "bash"
    assert slate_step["env"] == dict(step["env"], JOBS="slate-check")
    assert slate_step["run"] == step["run"], "all pickers must use the same identity and fallback contract"
    assert slate["slate-check"]["needs"] == ["writer-cases", "windows-cases"]
    assert slate["slate-check"]["runs-on"] == "${{ fromJSON(needs.windows-cases.outputs.runners || '{}')['slate-check'] || 'ubuntu-24.04' }}"
    keys = ["slate-check"]
    exercise()
    total_picks += len(keys)

print(f"runner gate: {total_picks} independent job picks, {cases} identity and HTTP cases passed")
PY
