#!/usr/bin/env bash
# Matrix recipes write an object ID, a security descriptor and file
# timestamps with this crate, Windows reads each back and chkdsk grades the
# volume, and the Slate tier captures each before Windows mounts it (#1065,
# part of #688). #829 found no `cli-*` recipe writing those families, so the
# Slate tier had nothing of theirs to capture.
#
# This reads the workflow, the matrix, the harness ops, cli-oracle.sh,
# win-cli-verify.ps1 and the harness binary's dispatch as data; what the
# recipes write is graded by Windows in the matrix job, and the capture itself
# is slate-writer-capture.sh's contract.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fails=0

python3 - "$ROOT" <<'PY' || fails=$((fails + 1))
import json, re, sys, tomllib
from pathlib import Path
root = Path(sys.argv[1])
workflow = (root / ".github/workflows/slate-oracle.yml").read_text()
matrix = json.loads((root / "test-matrix.json").read_text())
scenarios = matrix.get("scenarios", matrix)
harness = tomllib.loads((root / "fs-windows-test-harness.toml").read_text())
ops = harness.get("ops", {})
cli_group = harness.get("groups", {}).get("cli", [])
oracle = (root / "scripts/cli-oracle.sh").read_text()
verify = (root / "scripts/fs-windows-test-harness/win-cli-verify.ps1").read_text()
dispatch = (root / "src/bin/rust_ntfs/main.rs").read_text()
problems = []

job = re.search(r"\n  writer-cases:\n(.*?)\n  [\w-]+:\n", workflow, re.S)
step = job and re.search(r"run-host-steps \\\n(.*?)>tmp/logs/slate-writer-capture\.log", job.group(1), re.S)
if not step:
    sys.exit("FAIL the writer-cases job no longer runs run-host-steps into slate-writer-capture.log")
captured = re.findall(r"--scenario (\S+)", step.group(1))

def bodies(name):
    """The cli-oracle.sh verb bodies the scenario's recipe runs."""
    found = []
    for s in scenarios.get(name, {}).get("recipe", []):
        verb = re.match(r"bash scripts/cli-oracle\.sh (\S+) ", ops.get(s.get("op"), {}).get("command", ""))
        if not verb:
            continue
        body = re.search(rf"\n    {re.escape(verb.group(1))}\)\n(.*?)\n        ;;\n", oracle, re.S)
        if body:
            found.append(body.group(1))
    return "\n".join(found)

# Each family: the harness subcommand that writes it, and the manifest kind
# Windows reads it back by.
families = {
    "object ID": ("objid", "objid"),
    "security descriptor": ("security-id", "acl"),
    "timestamps": ("times", "times"),
}
chosen = {}
for family, (verb, kind) in families.items():
    if not re.search(rf'^\s*"{re.escape(verb)}" => ', dispatch, re.M):
        problems.append(f"the harness binary has no `rust-ntfs {verb}` subcommand to write the {family} family")
    if not re.search(rf"^\s*'{re.escape(kind)}' \{{", verify, re.M):
        problems.append(f"win-cli-verify.ps1 does not read a `{kind}` manifest line back for the {family} family")
    writers = [n for n in captured
               if re.search(rf'"\$HARNESS" {re.escape(verb)} "\$image" ', bodies(n))
               and re.search(rf"printf '{re.escape(kind)}\\t", bodies(n))]
    if not writers:
        problems.append(f"no captured scenario writes the {family} family with `rust-ntfs {verb}` "
                        f"and records a `{kind}` manifest line for Windows")
        continue
    chosen[family] = writers[0]

# One scenario per family, so a family that breaks fails by its own name.
if len(set(chosen.values())) != len(chosen):
    problems.append(f"the families share a scenario: {chosen}")

for family, name in chosen.items():
    recipe = scenarios[name].get("recipe", [])
    names = [s.get("op") for s in recipe]
    if "win-cli-verify" not in names:
        problems.append(f"{name} ({family}): Windows does not read the manifest back (win-cli-verify)")
    if not any(s.get("op") == "win-chkdsk" and "readonly" in s.get("modes", "")
               and s.get("verdict_shape") == "Clean" for s in recipe):
        problems.append(f"{name} ({family}): no clean read-only win-chkdsk lane")
    if name not in cli_group:
        problems.append(f"{name} ({family}) is not in the cli group CI's Windows job runs")

if "tests/scripts/slate-metadata-recipes.sh" not in workflow:
    problems.append("this guard is not in the slate-oracle workflow's path filter")

for p in problems:
    print(f"FAIL {p}")
sys.exit(1 if problems else 0)
PY

if [ "$fails" -gt 0 ]; then
    echo "slate-metadata-recipes.sh: $fails checks failed"
    exit 1
fi
echo 'slate-metadata-recipes.sh: all checks passed'
