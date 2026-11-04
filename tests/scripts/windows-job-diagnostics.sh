#!/usr/bin/env bash
# A green `Windows chkdsk execution` job has to keep what every scenario said
# (#1245). The harness runner removes test-diagnostics/matrix/ at the start of
# every run, and the Windows ops write their evidence to the VM, so a job of
# many run-matrix.sh steps keeps only what is carried out of the way first.
#
# This reads ci.yml as data and runs the job's own bash steps, in the job's
# order and under the job's own `if:`, in a sandbox: the real run-matrix.sh and
# the real diagnostics scripts, over a stand-in harness that replaces
# test-diagnostics/matrix/ as the runner does and a stand-in scp over a stand-in
# VM directory. Every harness run stamps its step output and its VM report with
# the number of that run, and the Upload step's own paths then say what the
# artifact would hold. The real artifact exists only on a Windows runner.
#
# A missing yaml module fails here: it is what the job is read with.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

command -v python3 >/dev/null 2>&1 || {
    echo 'FAIL python3 is required'
    exit 1
}
python3 -c 'import yaml' 2>/dev/null || {
    echo 'FAIL the python3 yaml module is required (pip install pyyaml)'
    exit 1
}

tmp="$(mktemp -d "${TMPDIR:-/tmp}/windows-job-diagnostics.XXXXXX")"
trap 'rm -r "$tmp"' EXIT

python3 - "$ROOT" "$tmp" <<'PY'
import fnmatch
import json
import os
import re
import shutil
import stat
import subprocess
import sys

import yaml

root, tmp = sys.argv[1:3]
fails = []


def fail(message):
    fails.append(message)
    print(f"FAIL {message}")


job = yaml.safe_load(open(f"{root}/.github/workflows/ci.yml", encoding="utf-8"))["jobs"][
    "validate-mkfs-windows-run"
]
steps = job["steps"]
job_env = job.get("env") or {}
keep = job_env.get("MATRIX_KEEP_DIAG")

TOUCHES = ("run-matrix.sh", "matrix-fetch-diag.sh", "matrix-keep-diag.sh", "quota-enforce.txt")
bash_steps = [
    s for s in steps if s.get("shell") == "bash" and any(t in s.get("run", "") for t in TOUCHES)
]
matrix_steps = [s for s in bash_steps if "run-matrix.sh" in s["run"]]
upload = next(
    (
        s
        for s in steps
        if "upload-artifact" in s.get("uses", "")
        and s.get("with", {}).get("name", "").startswith("ntfs-windows-diag")
    ),
    None,
)
seal = next((s for s in bash_steps if "matrix-keep-diag.sh seal" in s["run"]), None)

# The job, as data.
if not matrix_steps:
    fail("the job has no run-matrix.sh step, so this test reads nothing")
if not keep:
    fail("the job does not set MATRIX_KEEP_DIAG, so each run-matrix.sh step replaces the last")
elif not str(keep).startswith("test-diagnostics/"):
    fail(f"MATRIX_KEEP_DIAG is {keep!r}, outside test-diagnostics/")
for s in steps:
    if "MATRIX_KEEP_DIAG" in (s.get("env") or {}):
        fail(f"step {s.get('name')!r} overrides MATRIX_KEEP_DIAG")
    if "run-tests.sh" in s.get("run", ""):
        fail(f"step {s.get('name')!r} runs the harness past run-matrix.sh")
    if keep and str(keep) in s.get("run", ""):
        fail(f"step {s.get('name')!r} touches the kept groups by name")
if upload is None:
    fail("the job has no diagnostics upload")
else:
    paths = upload["with"]["path"].split()
    if "always()" not in str(upload.get("if", "")):
        fail("the diagnostics upload does not run whatever the job did")
    if "test-diagnostics/matrix/" not in paths:
        fail("the upload omits the last group, test-diagnostics/matrix/")
    if keep and f"{keep}/" not in paths:
        fail(f"the upload omits the kept groups, {keep}/")
    if not {"!**/*.img*", "!**/*.vhd*"} <= set(paths):
        fail("the upload does not exclude disk images by name")
    if "retention-days" not in upload["with"]:
        fail("the upload has no retention bound")
    if steps.index(upload) < max(steps.index(s) for s in bash_steps):
        fail("a matrix step runs after the diagnostics upload, so the upload cannot hold it")
if seal is None:
    fail("no step holds the artifact to its size and to no disk image before the upload")
else:
    if "always()" not in str(seal.get("if", "")):
        fail("the seal step does not run whatever the job did")
    if seal.get("continue-on-error") is not True or not seal.get("id"):
        fail("the seal step can stop the upload: it needs continue-on-error and an id")
    if upload is not None and steps.index(seal) > steps.index(upload):
        fail("the seal step runs after the upload, so an image would already have left")
    gate = next((s for s in steps if s.get("name", "").startswith("Fail the job")), {})
    if seal.get("id") and f"steps.{seal['id']}.outcome" not in str(gate.get("if", "")):
        fail("the job's final gate does not read the seal step's verdict")
if any(s.get("with", {}).get("name") == "ntfs-quota-enforce-measurement" for s in steps):
    fail("ntfs-quota-enforce-measurement is a second upload beside the job's diagnostics")

STANDIN_HARNESS = r"""
import json, os, shutil, sys
args = [a for a in sys.argv[1:] if not a.startswith("--")]
filt = args[0]
matrix = json.load(open("test-matrix.json", encoding="utf-8"))["scenarios"]
names = sorted(n for n, s in matrix.items() if filt in n and s.get("recipe"))
shutil.rmtree("test-diagnostics/matrix", ignore_errors=True)
counter = os.environ["SANDBOX"] + "/runs"
for name in names:
    n = int(open(counter).read()) + 1 if os.path.exists(counter) else 1
    open(counter, "w").write(str(n))
    scenario = f"test-diagnostics/matrix/{name}"
    os.makedirs(f"{scenario}/step-00")
    open(f"{scenario}/recipe.json", "w").write('{"steps": [\n  {"host": "vm"}\n]}\n')
    open(f"{scenario}/step-00/stdout.txt", "w").write(f"run {n}\n")
    vm = os.environ["SANDBOX"] + f"/vm/diag/{name}"
    shutil.rmtree(vm, ignore_errors=True)
    os.makedirs(vm)
    open(f"{vm}/report.txt", "w").write(f"run {n}\n")
    if "quota-enforce" in name and "NO_QUOTA_REPORT" not in os.environ:
        open(f"{vm}/quota-enforce.txt", "w").write("hresult=0x80070070\n")
    if os.environ.get("VM_LITTER") == name:
        open(f"{vm}/nfs-left-behind.img", "w").write("not a real image\n")
        with open(f"{vm}/left-behind.bin", "wb") as f:
            f.truncate(5 * 1024 * 1024)
    if name in os.environ.get("VM_LARGE", "").split():
        with open(f"{vm}/large-report.txt", "wb") as f:
            f.truncate(3 * 1024 * 1024)
    open(os.environ["SANDBOX"] + "/harness.log", "a").write(f"{n} {name}\n")
with open(".test-env", "w") as f:
    f.write('export VM_HOST="runneradmin@localhost"\nexport VM_WORKDIR="C:/matrix"\n')
sys.exit(1 if filt == os.environ.get("HARNESS_FAIL") else 0)
"""

STANDIN_SCP = r"""#!/usr/bin/env bash
# The last two arguments are host:/workdir/diag/<scenario>/. and the local vm/.
for a in "$@"; do from="$to"; to="$a"; done
name="${from%/.}"; name="${name##*/}"
echo "$name" >> "$SANDBOX/scp.log"
[ "$name" != "${SCP_REFUSES:-}" ] || exit 1
[ -d "$SANDBOX/vm/diag/$name" ] || exit 1
cp -R "$SANDBOX/vm/diag/$name/." "$to"
"""


def write_exec(path, text):
    with open(path, "w", encoding="utf-8") as f:
        f.write(text)
    os.chmod(path, os.stat(path).st_mode | stat.S_IXUSR)


def sandbox(name):
    box = f"{tmp}/{name}"
    consumer = f"{box}/consumer"
    for d in (
        f"{consumer}/scripts",
        f"{box}/fs-windows-test-harness/scripts",
        f"{box}/rust-fs-core/scripts",
        f"{box}/bin",
        f"{box}/vm/diag",
    ):
        os.makedirs(d)
    for script in ("run-matrix.sh", "matrix-fetch-diag.sh", "matrix-keep-diag.sh"):
        if os.path.exists(f"{root}/scripts/{script}"):
            shutil.copy(f"{root}/scripts/{script}", f"{consumer}/scripts/{script}")
    shutil.copy(f"{root}/test-matrix.json", consumer)
    with open(f"{box}/harness.py", "w", encoding="utf-8") as f:
        f.write(STANDIN_HARNESS)
    write_exec(
        f"{box}/fs-windows-test-harness/scripts/run-tests.sh",
        f'#!/usr/bin/env bash\nexec python3 "{box}/harness.py" "$@"\n',
    )
    # tier.sh's own work is budgeting a log, which nothing here reads.
    write_exec(
        f"{box}/rust-fs-core/scripts/tier.sh",
        '#!/usr/bin/env bash\nwhile [ "$1" != -- ]; do shift; done\nshift\nexec "$@"\n',
    )
    write_exec(f"{box}/bin/scp", STANDIN_SCP)
    write_exec(f"{box}/bin/ssh", "#!/usr/bin/env bash\nexit 255\n")
    write_exec(f"{box}/bin/uname", "#!/usr/bin/env bash\necho MINGW64_NT-10.0-20348\n")
    write_exec(f"{box}/bin/cygpath", f'#!/usr/bin/env bash\necho "{box}/key"\n')
    return box, consumer


def base_env(box):
    env = {k: v for k, v in os.environ.items() if k not in ("HOST_IMAGE_DIR", "SSH_KEY", "VM_HOST", "VM_WORKDIR")}
    env.update(SANDBOX=box, PATH=f"{box}/bin:{os.environ['PATH']}", HOME=box)
    return env


def should_run(step, failed, outcomes):
    condition = str(step.get("if", ""))
    if not condition:
        return not failed
    if condition == "failure()":
        return failed
    if condition == "always()":
        return True
    needs = re.fullmatch(r"always\(\) && steps\.(\w+)\.outcome != 'skipped'", condition)
    if needs:
        return outcomes.get(needs.group(1), "skipped") != "skipped"
    fail(f"step {step.get('name')!r} has an if: this test does not model: {condition}")
    return True


def run_job(name, **extra):
    """Run the job's matrix steps. Returns (consumer dir, step name -> (outcome, output))."""
    box, consumer = sandbox(name)
    env = base_env(box)
    env.update({k: str(v) for k, v in job_env.items()})
    env.update(extra)
    failed = False
    outcomes, results = {}, {}
    for step in bash_steps:
        if not should_run(step, failed, outcomes):
            results[step["name"]] = ("skipped", "")
            if step.get("id"):
                outcomes[step["id"]] = "skipped"
            continue
        step_env = dict(env)
        status = "failure" if failed else "success"
        for key, value in (step.get("env") or {}).items():
            step_env[key] = str(value).replace("${{ job.status }}", status)
        run = subprocess.run(
            ["bash", "--noprofile", "--norc", "-eo", "pipefail", "-c", step["run"]],
            cwd=consumer,
            env=step_env,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            errors="replace",
        )
        outcome = "success" if run.returncode == 0 else "failure"
        results[step["name"]] = (outcome, run.stdout)
        if step.get("id"):
            outcomes[step["id"]] = outcome
        if outcome == "failure" and step.get("continue-on-error") is not True:
            failed = True
    return box, consumer, results


def artifact(consumer):
    """The files the job's Upload step would take, by its own include and exclude paths."""
    if upload is None:
        return {}
    patterns = upload["with"]["path"].split()
    excludes = [p[1:].replace("**/", "") for p in patterns if p.startswith("!")]
    held = {}
    for include in (p for p in patterns if not p.startswith("!")):
        for parent, _, files in os.walk(f"{consumer}/{include}"):
            for f in files:
                if any(fnmatch.fnmatch(f, e) for e in excludes):
                    continue
                path = f"{parent}/{f}"
                held[os.path.relpath(path, consumer)] = path
    return held


def harness_runs(box):
    log = f"{box}/harness.log"
    if not os.path.exists(log):
        return []
    return [line.split() for line in open(log, encoding="utf-8")]


def holds_run(held, name, n, leaf):
    want = f"run {n}\n"
    return any(
        rel.endswith(f"/{name}/{leaf}") and open(path, encoding="utf-8").read() == want
        for rel, path in held.items()
    )


def step_named(needle):
    return next((s["name"] for s in bash_steps if needle in s["run"]), None)


# ── A green job: every run of every scenario, its step output and its VM report.
box, consumer, results = run_job("green")
held = artifact(consumer)
runs = harness_runs(box)
if not runs:
    fail("green job: the stand-in harness never ran")
for name, (outcome, output) in results.items():
    if outcome != "success":
        fail(f"green job: step {name!r} ended {outcome}:\n{output[-600:]}")
lost_step = [f"{n}:{s}" for n, s in runs if not holds_run(held, s, n, "step-00/stdout.txt")]
lost_vm = [f"{n}:{s}" for n, s in runs if not holds_run(held, s, n, "vm/report.txt")]
if lost_step:
    fail(f"green job: the artifact lost the step output of {len(lost_step)} of {len(runs)} runs, first {lost_step[0]}")
if lost_vm:
    fail(f"green job: the artifact holds no VM report for {len(lost_vm)} of {len(runs)} runs, first {lost_vm[0]}")
if not any(rel.endswith("quota-enforce.txt") for rel in held):
    fail("green job: the artifact does not hold quota-enforce.txt")

# ── A scenario fails: the steps after it do not run, and what the failure path
# always kept is still at test-diagnostics/matrix/<scenario>/, with earlier
# groups kept beside it.
FAILS = "win-format-win-quota-mac-write-win-chkdsk"
box, consumer, results = run_job("red", HARNESS_FAIL=FAILS)
held = artifact(consumer)
runs = harness_runs(box)
failing = next((n for n, s in runs if s == FAILS), None)
if results.get(step_named(FAILS), ("",))[0] != "failure":
    fail("failing job: run-matrix.sh did not pass on the harness's failure")
if runs and runs[-1][1] != FAILS:
    fail(f"failing job: {runs[-1][1]} ran after the failure")
for leaf in ("step-00/stdout.txt", "vm/report.txt"):
    if failing is None or f"test-diagnostics/matrix/{FAILS}/{leaf}" not in held:
        fail(f"failing job: the failed scenario's {leaf} is not at test-diagnostics/matrix/")
earlier = [(n, s) for n, s in runs if s != FAILS]
lost = [s for n, s in earlier if not holds_run(held, s, n, "vm/report.txt")]
if lost:
    fail(f"failing job: {len(lost)} earlier scenarios lost their VM report, first {lost[0]}")
if seal is not None and results[seal["name"]][0] != "success":
    fail(f"failing job: a partial run failed the seal step:\n{results[seal['name']][1][-600:]}")

# ── What must not reach an artifact, and an unreachable VM, on a run the
# scenarios passed: the scenario's step stays green, and the seal step names it.
LITTER = "win-format-win-quota-mac-write-win-chkdsk"
REFUSED = "win-format-win-quota-mac-create-win-chkdsk"
LARGE = "mac-format-mkdir-rmdir-win-chkdsk win-format-win-quota-mac-delete-win-chkdsk mac-format-mac-objid-win-chkdsk"
box, consumer, results = run_job("bad", VM_LITTER=LITTER, SCP_REFUSES=REFUSED, VM_LARGE=LARGE)
held = artifact(consumer)
if results.get(step_named(LITTER), ("",))[0] != "success":
    fail("bad job: a scenario's step changed its verdict over its diagnostics")
if results.get(step_named(REFUSED), ("",))[0] != "success":
    fail("bad job: an unreachable VM failed a scenario that passed")
for rel in held:
    if rel.endswith((".img", "left-behind.bin")):
        fail(f"bad job: {rel} reached the artifact")
if seal is not None:
    outcome, output = results[seal["name"]]
    if outcome != "failure":
        fail("bad job: the seal step passed")
    for needle, what in (
        ("nfs-left-behind.img", "a disk image by name"),
        ("left-behind.bin", "a file the size of a disk image"),
        (REFUSED, "the scenario whose VM report did not come home"),
        ("budget", "the artifact over its budget"),
    ):
        if needle not in output:
            fail(f"bad job: the seal step does not name {what} ({needle}):\n{output[-600:]}")

# ── The measurement's own check still fails when the report did not come home.
box, consumer, results = run_job("no-quota-report", NO_QUOTA_REPORT="1")
quota_check = next((s["name"] for s in bash_steps if "quota-enforce.txt" in s["run"]), None)
if quota_check is None or results.get(quota_check, ("",))[0] != "failure":
    fail("a missing quota-enforce.txt did not fail its check")

# ── Everywhere else: chore matrix sets nothing, and run-matrix.sh is what it was.
box, consumer = sandbox("unset")
env = base_env(box)
env.pop("MATRIX_KEEP_DIAG", None)
filters = ("mac-format-mac-symlink-win-chkdsk", "mac-format-mac-objid-win-chkdsk")
for f in filters:
    done = subprocess.run(["bash", "scripts/run-matrix.sh", f], cwd=consumer, env=env,
                          stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    if done.returncode != 0:
        fail(f"unset: run-matrix.sh {f} exited {done.returncode}")
if os.path.exists(f"{box}/scp.log"):
    fail("unset: run-matrix.sh fetched from the VM without being asked")
if sorted(os.listdir(f"{consumer}/test-diagnostics")) != ["matrix"]:
    fail(f"unset: run-matrix.sh kept something: {os.listdir(consumer + '/test-diagnostics')}")
if not os.path.isdir(f"{consumer}/test-diagnostics/matrix/{filters[1]}"):
    fail("unset: the last run's diagnostics are not at test-diagnostics/matrix/")
env["HARNESS_FAIL"] = filters[1]
done = subprocess.run(["bash", "scripts/run-matrix.sh", filters[1]], cwd=consumer, env=env,
                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
if done.returncode == 0:
    fail("unset: run-matrix.sh swallowed the harness's failure")

sys.exit(1 if fails else 0)
PY
[ "$?" -eq 0 ] || exit 1
printf 'windows-job-diagnostics.sh: all checks passed\n'
