#!/usr/bin/env bash
# A workflow job never imports a Python module that nothing in the job installs.
#
# The gated antimatter-vps runner image has python3 and no PyYAML, where the
# hosted image has both, so a step that imports yaml passed on a hosted runner
# and failed with ModuleNotFoundError on the gated one (merge-group run
# 44525486099, "The Windows 4Kn fixture lifecycle is bounded"). A job states
# what it needs: PyYAML comes from the user-tools action (`tools: ... pyyaml`)
# or from the job's own apt/pip install line.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
python3 -c 'import yaml' 2>/dev/null || {
  echo "FAIL: python3 has no yaml module; run \`chore tools pyyaml\`" >&2
  exit 1
}
python3 - "$ROOT" <<'PY'
import pathlib
import re
import sys

import yaml

root = pathlib.Path(sys.argv[1])

# module -> spellings of the install line that provide it
PROVIDERS = {"yaml": ("pyyaml", "python3-yaml")}

local = {p.stem for p in root.rglob("*.py") if ".git" not in p.parts and "target" not in p.parts}
IMPORT = re.compile(r"^\s*(?:import|from)\s+([A-Za-z_]\w*)", re.M)
SCRIPT = re.compile(r"(?:tests/(?:scripts|python)|scripts)/[\w./-]+\.(?:sh|py)")


def imports(text):
    return {m for m in IMPORT.findall(text)}


def third_party(modules):
    return sorted(m for m in modules if m not in sys.stdlib_module_names and m not in local)


problems = []
checked = 0
for workflow in sorted((root / ".github/workflows").glob("*.yml")):
    document = yaml.safe_load(workflow.read_text())
    for job_name, job in (document.get("jobs") or {}).items():
        steps = job.get("steps") or []
        # Everything the job installs: run lines and the user-tools `tools:` input.
        installs = "\n".join(
            str(step.get("run", "")) + "\n" + str((step.get("with") or {}).get("tools", ""))
            for step in steps
        ).lower()
        for index, step in enumerate(steps):
            run = str(step.get("run", ""))
            if not run:
                continue
            wanted = set()
            if re.search(r"python3?\b", run):
                wanted |= imports(run)
            for match in SCRIPT.findall(run):
                path = root / match
                if path.is_file():
                    wanted |= imports(path.read_text(errors="replace"))
            checked += 1
            for module in third_party(wanted):
                providers = PROVIDERS.get(module)
                where = f"{workflow.name} job {job_name!r} step {index + 1} ({step.get('name', run.splitlines()[0][:50])})"
                if providers is None:
                    problems.append(f"{where}: imports {module}, which no rule here says how to install; add it to PROVIDERS")
                elif not any(p in installs for p in providers):
                    problems.append(f"{where}: imports {module}, but the job installs none of {', '.join(providers)}")

for problem in problems:
    print(f"FAIL: {problem}", file=sys.stderr)
assert checked, "no workflow step was examined"
sys.exit(1 if problems else 0)
PY
echo "PASS: every Python module a workflow step imports is installed by its job"
