#!/usr/bin/env bash
# The #826 evidence consumer fails closed without a Windows VM.
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 1
python3 - <<'PY'
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("grader", "scripts/check-attrlist-records-chkdsk.py")
grader = importlib.util.module_from_spec(spec)
spec.loader.exec_module(grader)


class Grades(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        for case in grader.CASES:
            self.evidence(case, case != "clean")

    def evidence(self, case, damaged):
        folder = self.root / case
        folder.mkdir(exist_ok=True)
        code = 3 if damaged else 0
        mode = {"exit": code, "state": "failed" if damaged else "scanned"}
        verdict = {"passed": not damaged, "verdict_shape": "clean", "modes": {"readonly": mode}, "exits": {"readonly": code}}
        (folder / "verdict.json").write_text(json.dumps(verdict))
        (folder / "chkdsk-readonly-exit.txt").write_text(str(code))
        (folder / "chkdsk-readonly.txt").write_text("Errors found.  CHKDSK cannot continue in read-only mode." if damaged else "Windows has scanned the file system and found no problems.")
        (self.root / f"{case}.findings.txt").write_text("BadAttributeList" if case != "clean" else "CheckReport { findings: [] }")

    def grade(self):
        with contextlib.redirect_stdout(io.StringIO()):
            return grader.grade(self.root)

    def test_four_complete_grades_pass(self):
        self.assertTrue(self.grade())
        self.assertEqual(len(json.loads((self.root / "comparisons.json").read_text())), 4)

    def test_every_verdict_is_required(self):
        for case in grader.CASES:
            with self.subTest(case=case):
                target = self.root / case / "verdict.json"
                original = target.read_text()
                target.unlink()
                self.assertFalse(self.grade())
                target.write_text(original)

    def test_clean_corruption_is_retained_as_disagreement(self):
        for case in grader.CASES[1:]:
            with self.subTest(case=case):
                self.evidence(case, False)
                self.assertFalse(self.grade())
                result = next(x for x in json.loads((self.root / "comparisons.json").read_text()) if x["case"] == case)
                self.assertEqual(result["actual"], "clean")
                self.assertFalse(result["agrees"])
                self.evidence(case, True)

    def test_damaged_control_fails(self):
        self.evidence("clean", True)
        self.assertFalse(self.grade())

    def test_all_raw_reports_and_checker_findings_are_required(self):
        for relative in ("sequence/chkdsk-readonly.txt", "sequence/chkdsk-readonly-exit.txt", "sequence.findings.txt"):
            with self.subTest(file=relative):
                target = self.root / relative
                original = target.read_text()
                target.unlink()
                self.assertFalse(self.grade())
                target.write_text(original)

    def test_process_failure_is_not_damage(self):
        (self.root / "sequence/chkdsk-readonly.txt").write_text("Cannot open volume for direct access.")
        self.assertFalse(self.grade())

    def test_repair_and_missing_passes_fail(self):
        target = self.root / "sequence/verdict.json"
        original = json.loads(target.read_text())
        for modes in ({}, {**original["modes"], "/F /X": {"exit": 0, "state": "scanned"}}):
            with self.subTest(modes=modes):
                verdict = {**original, "modes": modes}
                target.write_text(json.dumps(verdict))
                self.assertFalse(self.grade())

    def test_inconsistent_exit_and_unscanned_pass_fail(self):
        target = self.root / "sequence/verdict.json"
        original = target.read_text()
        for field, value in (("exit", 0), ("state", "not-scanned")):
            with self.subTest(field=field):
                verdict = json.loads(original)
                verdict["modes"]["readonly"][field] = value
                target.write_text(json.dumps(verdict))
                self.assertFalse(self.grade())


unittest.main()
PY
rc=$?
[ "$rc" -eq 0 ] || exit "$rc"
echo 'attrlist-records-chkdsk: all checks passed'
