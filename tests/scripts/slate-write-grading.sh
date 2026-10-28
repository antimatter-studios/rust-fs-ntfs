#!/usr/bin/env bash
# Windows grades every Slate-written copy (#864, part of #690): a Windows job
# of the advisory Slate workflow takes the slate-oracle-writes artifact from
# the slate-check job and runs build-slate-oracle-cases.ps1 -SlateWrites on
# it. That writes slate-writes-graded.tsv, one row per row of the writer
# table, grades each copy with read-only chkdsk, captures the read manifest
# only for a copy Windows accepts, and names a rejected copy in the job
# summary as a Slate writer defect without failing the run. This reads the
# workflow and the script as data; Windows itself runs only in CI.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fails=0

python3 - "$ROOT" <<'PY' || fails=$((fails + 1))
import re, sys
from pathlib import Path
root = Path(sys.argv[1])
problems = []

workflow = (root / ".github/workflows/slate-oracle.yml").read_text()
script = (root / "test-disks/build-slate-oracle-cases.ps1").read_text()

job = re.search(r"\n  slate-writes-windows:\n(.*?)(?=\n  [A-Za-z0-9_-]+:\n|\Z)", workflow, re.S)
job = job.group(1) if job else ""
if not job:
    problems.append("the workflow has no slate-writes-windows job")
for want, why in [
    (r"\n    needs: slate-check\n", "does not run after slate-check, which writes the copies"),
    (r"\n    if: \$\{\{ !cancelled\(\) \}\}\n", "is skipped when the comparison disagreed"),
    (r"\n    runs-on: windows-latest\n", "does not run on Windows"),
    (r"\n    timeout-minutes: \d+\n", "has no timeout"),
    (r"name: slate-oracle-writes-\$\{\{ github\.sha \}\}\n\s+path: tmp/slate-writes-input\n",
     "does not download the slate-oracle-writes artifact"),
    (r"build-slate-oracle-cases\.ps1\s+-OutDir tmp/slate-write-grades\s+"
     r"-SlateWrites tmp/slate-writes-input/writes/slate-writes\.tsv\s+"
     r"-SlateWriteImages tmp/slate-writes-input/writes/images\n",
     "does not grade the writer table's copies with build-slate-oracle-cases.ps1 -SlateWrites"),
]:
    if not re.search(want, job):
        problems.append(f"the slate-writes-windows job {why}")
upload = re.search(r"name: slate-oracle-write-grades-\$\{\{ github\.sha \}\}\n(.*?)if-no-files-found: error",
                   job, re.S)
if not upload:
    problems.append("the slate-writes-windows job does not upload slate-oracle-write-grades")
else:
    for want in ["tmp/slate-write-grades/slate-writes-graded.tsv", "tmp/slate-write-grades/images/*.img",
                 "tmp/slate-write-grades/chkdsk/", "tmp/slate-write-grades/windows-read/"]:
        if want not in upload.group(1):
            problems.append(f"the write-grades artifact does not carry {want}")
for path in ["tests/scripts/slate-write-grading.sh"]:
    if f"      - {path}\n" not in workflow:
        problems.append(f"a change to {path} does not run the workflow")

# The script's -SlateWrites mode.
for want, why in [
    (r"\[string\]\$SlateWrites\b", "has no -SlateWrites parameter"),
    (r"\[string\]\$SlateWriteImages\b", "has no -SlateWriteImages parameter"),
    (r'"case`timage`tsha256`twindows_verdict`twindows_exit"',
     "does not write the graded table's case, image, sha256, verdict and exit header"),
    (r"slate-writes-graded\.tsv", "does not write slate-writes-graded.tsv"),
    (r"'accepted'", "never marks a copy accepted"),
    (r"'slate-defect'", "never records a rejected copy as a Slate defect"),
    (r"\bnot-written\b", "drops a writer row that left no copy instead of recording it"),
    (r"GITHUB_STEP_SUMMARY", "does not name Slate defects in the job summary"),
    (r"written_sha256", "does not check each copy against the SHA-256 the writer recorded"),
    (r"-ne \$written\.Count", "does not require one graded row per writer row"),
]:
    if not re.search(want, script):
        problems.append(f"build-slate-oracle-cases.ps1 {why}")
# The manifest is captured only for a copy Windows accepted, so chkdsk must
# run before the read capture.
chkdsk = script.find("& pwsh -NoProfile -File $winChkdsk")
capture = script.find("Get-SlateWindowsManifest -Root")
if chkdsk < 0 or capture < 0 or chkdsk > capture:
    problems.append("build-slate-oracle-cases.ps1 does not run chkdsk before the Windows read capture")
if not re.search(r"if \(-not \$case\.Slate -or \$passed\)", script):
    problems.append("build-slate-oracle-cases.ps1 captures a manifest for a copy Windows rejected")
if re.search(r"Slate defect.*throw|throw.*Slate defect", script):
    problems.append("build-slate-oracle-cases.ps1 fails the run on a Slate defect")

for problem in problems:
    print(f"FAIL {problem}")
sys.exit(1 if problems else 0)
PY

if [ "$fails" -gt 0 ]; then
    echo "slate-write-grading.sh: $fails of 1 checks failed"
    exit 1
fi
echo "slate-write-grading.sh: all checks passed"
